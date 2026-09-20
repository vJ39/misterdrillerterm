//! N人対戦のルーム参加フローとフルメッシュ確立(#275。docs/multiplayer-4p-room-design.md)。
//!
//! #274のフルメッシュlockstep(`BattleState::from_peer_streams`)が要求する「確立済みの
//! TCP接続N-1本」を用意するところを担う。主催者が参加者を集めて`RoomRoster`・対戦設定・
//! シード・開始時刻を配布し、全員が同じ`members`の並び(room内インデックス)を見て
//! C(n,2)本のメッシュ接続を張る。
//!
//! ロビーUI(ルーム作成・参加者一覧の表示・開始操作)は#276の範囲のため、ここは
//! ブロッキングI/Oで完結する関数群として置く(#276が非ブロッキングな状態機械でラップする)。
//! ルーム参加接続のaccept自体も、参加者が増えるたびに一覧を更新表示するという操作性の
//! ために呼び出し元の責務としている(設計書4節)。

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use rand::RngExt;

use crate::net::{self, BattleConfig, GameMessage, RoomMember, TCP_CONNECT_TIMEOUT_MS};

/// 主催者側。既に`JoinRoom`を受け取った各ゲストとの接続(`guest_room_streams`。
/// `JoinRoom`を受信した順=room内インデックス1,2,3...と対応)へ`RoomRoster`・対戦設定・
/// シード・開始時刻を配布し、フルメッシュのメッシュ接続(自分以外、room内インデックス順)を
/// 確立する(設計書4節)。
///
/// `guest_names`/`guest_mesh_addrs`は`guest_room_streams`と同じ順で、それぞれ`JoinRoom`の
/// `name`と、「接続元のIP+`JoinRoom`の`mesh_port`」を呼び出し元が組み立てたもの。
///
/// 戻り値の`HandshakeResult`は2人版と同じ型だが、`opponent_name`はN人版では意味を持た
/// ないため空にする(参加者名は`RoomRoster`の`members`が持ち、呼び出し元は自分が組み立てた
/// `guest_names`をそのまま使える)。
#[allow(dead_code)] // #276でロビーから呼ぶまではテストからのみ使う。
pub fn start_room_as_host(
    guest_room_streams: &mut [TcpStream],
    guest_names: &[String],
    guest_mesh_addrs: &[SocketAddr],
    my_name: &str,
    my_mesh_listener: &TcpListener,
    config: BattleConfig,
) -> io::Result<(Vec<TcpStream>, net::HandshakeResult)> {
    if guest_room_streams.len() != guest_names.len()
        || guest_room_streams.len() != guest_mesh_addrs.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ゲストの接続・名前・メッシュ宛先は同数で同じ順のはず",
        ));
    }

    // 主催者はroom内インデックス0。以降は`JoinRoom`を受け取った順。
    let mut members = Vec::with_capacity(guest_names.len() + 1);
    members.push(RoomMember {
        name: my_name.to_string(),
        mesh_addr: my_mesh_listener.local_addr()?,
    });
    for (name, &mesh_addr) in guest_names.iter().zip(guest_mesh_addrs) {
        members.push(RoomMember {
            name: name.clone(),
            mesh_addr,
        });
    }

    // シードは主催者がOS乱数から単独で決める(2人版`run_host_handshake`と同じ取り決め。
    // spec.md 12.2ステップ3)。開始時刻も同じ猶予を使う。
    let seed: u64 = rand::rng().random();
    let start_at_unix_ms = net::countdown_start_time_ms();

    for (guest_index, stream) in guest_room_streams.iter_mut().enumerate() {
        // `your_index`だけ送り先ごとに変える(名前が重複していても各参加者が自分を
        // 一意に特定できるようにするため。設計書2節)。
        net::write_message(
            stream,
            &GameMessage::RoomRoster {
                members: members.clone(),
                your_index: guest_index + 1,
            },
        )?;
        net::write_message(stream, &GameMessage::StartConfig(config))?;
        net::write_message(stream, &GameMessage::SeedAgree { seed })?;
        net::write_message(stream, &GameMessage::StartCountdown { start_at_unix_ms })?;
    }

    let streams = establish_full_mesh(&members, 0, my_name, my_mesh_listener)?;

    Ok((
        streams,
        net::HandshakeResult {
            opponent_name: String::new(),
            config,
            seed,
            start_at_unix_ms,
        },
    ))
}

/// 参加者側。主催者へ接続して`JoinRoom`を送り、`RoomRoster`以降を受け取ってから
/// フルメッシュを確立する(設計書5節)。
///
/// 戻り値は(自分以外とのメッシュ接続。room内インデックス順, 自分以外の名前を同じ順で
/// 並べたもの, ハンドシェイク結果)。`HandshakeResult::opponent_name`は主催者側と同じ理由で
/// 空にする(名前は2つ目の戻り値が持つ)。
#[allow(dead_code)] // #276でロビーから呼ぶまではテストからのみ使う。
pub fn join_room_as_guest(
    host_addr: SocketAddr,
    my_name: &str,
    my_mesh_listener: &TcpListener,
) -> io::Result<(Vec<TcpStream>, Vec<String>, net::HandshakeResult)> {
    let mesh_port = my_mesh_listener.local_addr()?.port();
    let mut room_stream =
        TcpStream::connect_timeout(&host_addr, Duration::from_millis(TCP_CONNECT_TIMEOUT_MS))?;
    net::write_message(
        &mut room_stream,
        &GameMessage::JoinRoom {
            name: my_name.to_string(),
            mesh_port,
        },
    )?;

    let (mut members, my_index) = match net::read_message(&mut room_stream)? {
        GameMessage::RoomRoster {
            members,
            your_index,
        } => (members, your_index),
        other => return Err(net::unexpected_message("RoomRoster", &other)),
    };
    // 参加者は必ず主催者(インデックス0)より後ろにいる。範囲外なら以降の役割決定
    // (どちらがTCPサーバ役か)が破綻するため、ここで弾く。
    if my_index == 0 || my_index >= members.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "your_index({my_index})がmembers({}人)の参加者の範囲に無い",
                members.len()
            ),
        ));
    }
    members[0].mesh_addr = resolve_host_mesh_addr(members[0].mesh_addr, host_addr);

    let config = match net::read_message(&mut room_stream)? {
        GameMessage::StartConfig(config) => config,
        other => return Err(net::unexpected_message("StartConfig", &other)),
    };
    let seed = match net::read_message(&mut room_stream)? {
        GameMessage::SeedAgree { seed } => seed,
        other => return Err(net::unexpected_message("SeedAgree", &other)),
    };
    let start_at_unix_ms = match net::read_message(&mut room_stream)? {
        GameMessage::StartCountdown { start_at_unix_ms } => start_at_unix_ms,
        other => return Err(net::unexpected_message("StartCountdown", &other)),
    };

    let streams = establish_full_mesh(&members, my_index, my_name, my_mesh_listener)?;
    let other_names = members
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != my_index)
        .map(|(_, member)| member.name.clone())
        .collect();

    // ルーム参加接続はここで用済み(設計書1節)。メッシュ確立まで開いたままにしておき、
    // この関数を抜けるところで閉じる。
    drop(room_stream);

    Ok((
        streams,
        other_names,
        net::HandshakeResult {
            opponent_name: String::new(),
            config,
            seed,
            start_at_unix_ms,
        },
    ))
}

/// `members`(room内インデックス順、自分を含む)と自分のインデックス`my_index`から、
/// 自分以外の全員とのメッシュ接続を確立する(設計書6節)。戻り値は`members`から自分を
/// 除いた順(`BattleState::from_peer_streams`が要求する、`games[1..]`と対応する順)。
///
/// 役割は「room内インデックスが小さい方がTCPサーバ役」という決定的な規則で決まるため、
/// 全員が同じ`members`を見ていれば通信なしに一致する(設計書3節)。
fn establish_full_mesh(
    members: &[RoomMember],
    my_index: usize,
    my_name: &str,
    mesh_listener: &TcpListener,
) -> io::Result<Vec<TcpStream>> {
    // この関数はブロッキングI/Oで完結する前提。ロビーのlistener(`lobby.rs`)は
    // 非ブロッキングに設定されているため、accept前にブロッキングへ戻す。
    mesh_listener.set_nonblocking(false)?;

    let mut connections: Vec<Option<TcpStream>> = members.iter().map(|_| None).collect();

    // 自分より小さいインデックスへは自分から繋ぐ(クライアント役)。相手は自分が選んで
    // いるのでインデックスは既知で、`Hello`は名前の確認(繋ぎ先の取り違え検出)に使う。
    for (index, member) in members.iter().enumerate().take(my_index) {
        let mut stream = TcpStream::connect_timeout(
            &member.mesh_addr,
            Duration::from_millis(TCP_CONNECT_TIMEOUT_MS),
        )?;
        // 2人版`run_client_handshake`と同じ順序(先に送ってから受け取る)。
        net::write_message(
            &mut stream,
            &GameMessage::Hello {
                name: my_name.to_string(),
            },
        )?;
        let peer_name = read_hello(&mut stream)?;
        if peer_name != member.name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "room内インデックス{index}には{}が居るはずだが{peer_name}が応答した",
                    member.name
                ),
            ));
        }
        connections[index] = Some(stream);
    }

    // 自分より大きいインデックスからの接続を受け入れる(サーバ役)。到着順はインデックス順
    // と一致しないため、`Hello`の名前を`members`と照合して挿入位置を決める。
    //
    // 自分より後ろに同名の参加者が複数いる場合、この照合では本人を区別できない
    // (まだ埋まっていない最初の一致へ入れる)。自分の特定は`your_index`で行える一方、
    // peerの特定は名前しか手がかりが無いという設計上の制約(#275設計書3節)。
    for _ in (my_index + 1)..members.len() {
        let (mut stream, _) = mesh_listener.accept()?;
        // listenerが非ブロッキングだった場合、環境によっては受理したストリームもそれを
        // 引き継ぐ(`lobby.rs`と同じ理由で明示的に戻す)。
        stream.set_nonblocking(false)?;
        // 2人版`run_host_handshake`と同じ順序(受け取ってから返す)。
        let peer_name = read_hello(&mut stream)?;
        net::write_message(
            &mut stream,
            &GameMessage::Hello {
                name: my_name.to_string(),
            },
        )?;

        let index = members
            .iter()
            .enumerate()
            .skip(my_index + 1)
            .find(|(index, member)| member.name == peer_name && connections[*index].is_none())
            .map(|(index, _)| index)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("接続してきた{peer_name}がmembersの未接続の参加者に見つからない"),
                )
            })?;
        connections[index] = Some(stream);
    }

    let mut streams = Vec::with_capacity(members.len().saturating_sub(1));
    for (index, connection) in connections.into_iter().enumerate() {
        if index == my_index {
            continue;
        }
        streams.push(connection.ok_or_else(|| {
            io::Error::other(format!(
                "room内インデックス{index}との接続が確立できていない"
            ))
        })?);
    }
    Ok(streams)
}

/// メッシュ接続の`Hello`を1件受け取り、相手の名前を返す。
fn read_hello(stream: &mut TcpStream) -> io::Result<String> {
    match net::read_message(stream)? {
        GameMessage::Hello { name } => Ok(name),
        other => Err(net::unexpected_message("Hello", &other)),
    }
}

/// 主催者の`mesh_addr`を、参加者が実際に繋がった宛先のIPで解決する。
///
/// 設計書に無い追加処理。主催者は自分の`mesh_addr`を`TcpListener::local_addr()`から作るが、
/// ロビーのlistenerは全インターフェース(0.0.0.0)にbindされるため、そのIPのままでは参加者が
/// 主催者へ繋げない。参加者はルーム参加接続で使った宛先で主催者に到達できることが確かなので、
/// IPが未指定(0.0.0.0 / ::)のときだけその宛先のIPへ差し替える(具体的なIPを広告している
/// 場合は、主催者が特定のNICで待ち受ける構成を壊さないようそのまま使う)。
fn resolve_host_mesh_addr(roster_addr: SocketAddr, host_addr: SocketAddr) -> SocketAddr {
    if roster_addr.ip().is_unspecified() {
        SocketAddr::new(host_addr.ip(), roster_addr.port())
    } else {
        roster_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::{BattleState, new_game_from_battle_config};
    use crate::game::{Game, InputAction};
    use crate::settings::Settings;
    use std::net::Ipv4Addr;
    use std::thread;

    /// テストのポンプ回数の上限。ループバックの配送待ちで何周か空回りするため実際の
    /// tick数より多めに取る(`battle.rs`のテストと同じ考え方)。
    const MAX_PUMPS: usize = 500;

    /// テスト用の対戦設定。盤面生成を軽くするため短いコース(20m)にする。
    fn test_config() -> BattleConfig {
        BattleConfig::from_settings(&Settings::default(), 20)
    }

    /// ループバックの空きポートで待ち受けるlistener(ルーム参加用・メッシュ接続用)。
    fn loopback_listener() -> TcpListener {
        TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap()
    }

    /// 1人ぶんのルーム参加結果。
    struct Participant {
        name: String,
        /// 自分以外とのメッシュ接続(room内インデックス順)。
        streams: Vec<TcpStream>,
        /// 自分以外の名前(`streams`と同じ順)。
        other_names: Vec<String>,
        handshake: net::HandshakeResult,
    }

    /// `names`(index 0が主催者)の人数でルームを1つ成立させ、room内インデックス順の
    /// 参加結果を返す。
    ///
    /// ルーム参加接続のacceptは呼び出し元の責務(設計書4節)なので、ここがその役を担う。
    /// ゲストのroom内インデックスは主催者が`JoinRoom`を受け取った順で決まるため、
    /// 順序を確定させるためゲストは1人ずつ参加させる(`JoinRoom`送信後は`RoomRoster`待ちで
    /// ブロックするので、次のゲストを起こす前にインデックスが確定する)。
    fn run_room(names: &[String]) -> Vec<Participant> {
        let room_listener = loopback_listener();
        let room_addr = room_listener.local_addr().unwrap();
        let host_mesh_listener = loopback_listener();

        let mut guest_threads = Vec::new();
        let mut guest_room_streams = Vec::new();
        let mut guest_names = Vec::new();
        let mut guest_mesh_addrs = Vec::new();

        for name in &names[1..] {
            let name = name.clone();
            guest_threads.push(thread::spawn(move || {
                let mesh_listener = loopback_listener();
                let (streams, other_names, handshake) =
                    join_room_as_guest(room_addr, &name, &mesh_listener).unwrap();
                Participant {
                    name,
                    streams,
                    other_names,
                    handshake,
                }
            }));

            // このゲストの`JoinRoom`を受け取ってからインデックスを確定させる。
            let (mut stream, peer_addr) = room_listener.accept().unwrap();
            let (join_name, mesh_port) = match net::read_message(&mut stream).unwrap() {
                GameMessage::JoinRoom { name, mesh_port } => (name, mesh_port),
                other => panic!("JoinRoomを待っていたが{other:?}を受信した"),
            };
            guest_names.push(join_name);
            guest_mesh_addrs.push(SocketAddr::new(peer_addr.ip(), mesh_port));
            guest_room_streams.push(stream);
        }

        let (host_streams, host_handshake) = start_room_as_host(
            &mut guest_room_streams,
            &guest_names,
            &guest_mesh_addrs,
            &names[0],
            &host_mesh_listener,
            test_config(),
        )
        .unwrap();

        let mut participants = vec![Participant {
            name: names[0].clone(),
            streams: host_streams,
            other_names: guest_names,
            handshake: host_handshake,
        }];
        for guest in guest_threads {
            participants.push(guest.join().unwrap());
        }
        participants
    }

    /// `p0`,`p1`,...という名前の`n`人。
    fn numbered_names(n: usize) -> Vec<String> {
        (0..n).map(|index| format!("p{index}")).collect()
    }

    /// 参加者`my_index`の`streams[stream_index]`が繋がっている相手のroom内インデックス
    /// (自分を除いた並びなので、自分以降は1つずれる)。
    fn peer_index(my_index: usize, stream_index: usize) -> usize {
        if stream_index < my_index {
            stream_index
        } else {
            stream_index + 1
        }
    }

    /// 参加者`my_index`の視点で、参加者`participant`の盤面が`games`の何番目にあるか
    /// (`games`は[自分, 自分以外(room内インデックス順)]の並び)。
    fn games_index_of(my_index: usize, participant: usize) -> usize {
        if participant == my_index {
            0
        } else if participant < my_index {
            participant + 1
        } else {
            participant
        }
    }

    /// 全員が自分のroom内インデックスを名乗り合い、`streams[i]`が期待した相手に
    /// 繋がっていることを確かめる(本数と順序の検証)。メッシュ接続を消費するため、
    /// 対戦を進めるテストとは別のルームで行う。
    fn assert_streams_are_in_room_index_order(participants: &mut [Participant]) {
        let total = participants.len();
        for (my_index, participant) in participants.iter_mut().enumerate() {
            assert_eq!(
                participant.streams.len(),
                total - 1,
                "参加者{my_index}は自分以外の全員と接続を持つはず"
            );
            for stream in participant.streams.iter_mut() {
                net::write_message(
                    stream,
                    &GameMessage::Hello {
                        name: format!("index{my_index}"),
                    },
                )
                .unwrap();
            }
        }

        for (my_index, participant) in participants.iter_mut().enumerate() {
            for (stream_index, stream) in participant.streams.iter_mut().enumerate() {
                let expected = peer_index(my_index, stream_index);
                let received = read_hello(stream).unwrap();
                assert_eq!(
                    received,
                    format!("index{expected}"),
                    "参加者{my_index}のstreams[{stream_index}]は参加者{expected}に繋がっているはず"
                );
            }
        }
    }

    #[test]
    fn a_three_player_room_connects_everyone_in_room_index_order() {
        let mut participants = run_room(&numbered_names(3));

        assert_streams_are_in_room_index_order(&mut participants);
    }

    #[test]
    fn a_four_player_room_connects_everyone_in_room_index_order() {
        let mut participants = run_room(&numbered_names(4));

        assert_streams_are_in_room_index_order(&mut participants);
    }

    #[test]
    fn a_two_player_room_goes_through_the_same_flow_with_a_single_connection() {
        // 2人でも同じ経路で成立する(主催者が接続を受け、参加者が繋ぐ側になる)。
        let mut participants = run_room(&numbered_names(2));

        assert_streams_are_in_room_index_order(&mut participants);
    }

    #[test]
    fn everyone_in_the_room_ends_up_with_the_same_config_seed_and_start_time() {
        let participants = run_room(&numbered_names(4));

        let host = &participants[0].handshake;
        assert_eq!(host.config, test_config(), "主催者は自分の設定を使うはず");
        for (index, participant) in participants.iter().enumerate().skip(1) {
            assert_eq!(
                participant.handshake.config, host.config,
                "参加者{index}は主催者の設定に従うはず"
            );
            assert_eq!(participant.handshake.seed, host.seed);
            assert_eq!(
                participant.handshake.start_at_unix_ms,
                host.start_at_unix_ms
            );
        }
    }

    #[test]
    fn everyone_learns_the_other_participants_names_in_room_index_order() {
        let names = numbered_names(4);
        let participants = run_room(&names);

        for (my_index, participant) in participants.iter().enumerate() {
            let expected: Vec<String> = names
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != my_index)
                .map(|(_, name)| name.clone())
                .collect();
            assert_eq!(
                participant.other_names, expected,
                "参加者{my_index}は自分以外の名前をroom内インデックス順で受け取るはず"
            );
            assert_eq!(participant.name, names[my_index]);
        }
    }

    #[test]
    fn a_guest_with_the_same_name_as_the_host_still_finds_itself_through_your_index() {
        // 主催者と同名の参加者がいても、`your_index`で自分を特定できるため役割
        // (どちらがTCPサーバ役か)が食い違わない。名前だけで自分を探すと、この参加者は
        // 主催者(インデックス0)を自分だと誤認して誰にも繋がなくなる。
        let names = vec!["dup".to_string(), "dup".to_string(), "solo".to_string()];

        let mut participants = run_room(&names);

        assert_streams_are_in_room_index_order(&mut participants);
        assert_eq!(participants[1].other_names, vec!["dup", "solo"]);
        assert_eq!(participants[2].other_names, vec!["dup", "dup"]);
    }

    #[test]
    fn the_established_mesh_drives_a_four_player_lockstep_battle() {
        // 確立した接続をそのまま`BattleState::from_peer_streams`へ渡し、数tick進めて
        // 全員の盤面が一致することを見る(#274との接続部分の確認)。
        const N: usize = 4;
        const TICKS: u32 = 6;
        let participants = run_room(&numbered_names(N));
        let seed = participants[0].handshake.seed;
        let config = participants[0].handshake.config;

        let mut states: Vec<BattleState> = participants
            .into_iter()
            .map(|participant| {
                let games: Vec<Game> = (0..N)
                    .map(|_| new_game_from_battle_config(seed, &config))
                    .collect();
                let mut player_names = vec![participant.name];
                player_names.extend(participant.other_names);
                BattleState::from_peer_streams(
                    games,
                    player_names,
                    participant.streams,
                    participant.handshake.start_at_unix_ms,
                )
                .unwrap()
            })
            .collect();

        for _ in 0..MAX_PUMPS {
            for (my_index, state) in states.iter_mut().enumerate() {
                // 参加者ごとに入力を変え、盤面が初期状態のまま揃う状況にしない。
                let action = match my_index % 3 {
                    0 => Some(InputAction::Drill),
                    1 => Some(InputAction::MoveRight),
                    _ => Some(InputAction::MoveLeft),
                };
                state.pump_frame(TICKS, action);
            }
            if states.iter().all(|state| state.current_net_tick() >= TICKS) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        for (my_index, state) in states.iter().enumerate() {
            assert_eq!(
                state.current_net_tick(),
                TICKS,
                "参加者{my_index}が目標tickまで進むはず"
            );
            assert_eq!(
                state.outcome(),
                None,
                "参加者{my_index}: この範囲では決着しないはず"
            );
        }

        // 同じ参加者の盤面が全員の視点で一致すること(接続の順序が食い違っていれば
        // 他人の入力を取り違えてここで壊れる)。
        for participant in 0..N {
            let expected = states[0].games[games_index_of(0, participant)].state_hash();
            for (my_index, state) in states.iter().enumerate().skip(1) {
                assert_eq!(
                    state.games[games_index_of(my_index, participant)].state_hash(),
                    expected,
                    "参加者{participant}の盤面が参加者0と参加者{my_index}で一致しない"
                );
            }
        }

        // 一致比較が自明に通る状況(全員初期状態のまま)になっていないことの裏取り。
        let initial = new_game_from_battle_config(seed, &config).state_hash();
        assert_ne!(
            states[0].games[0].state_hash(),
            initial,
            "少なくとも自分の盤面は初期状態から進んでいるはず"
        );
    }

    #[test]
    fn a_guest_rejects_a_first_reply_that_is_not_a_room_roster() {
        let room_listener = loopback_listener();
        let room_addr = room_listener.local_addr().unwrap();
        let fake_host = thread::spawn(move || {
            let (mut stream, _) = room_listener.accept().unwrap();
            assert!(matches!(
                net::read_message(&mut stream).unwrap(),
                GameMessage::JoinRoom { .. }
            ));
            net::write_message(&mut stream, &GameMessage::Bye).unwrap();
        });

        let mesh_listener = loopback_listener();
        let err = join_room_as_guest(room_addr, "guest", &mesh_listener).unwrap_err();
        fake_host.join().unwrap();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_guest_rejects_a_your_index_that_is_not_its_own_place_in_the_roster() {
        // 主催者(インデックス0)と、membersの範囲外はいずれも受け付けない。
        for your_index in [0, 2] {
            let room_listener = loopback_listener();
            let room_addr = room_listener.local_addr().unwrap();
            let fake_host = thread::spawn(move || {
                let (mut stream, _) = room_listener.accept().unwrap();
                net::read_message(&mut stream).unwrap();
                net::write_message(
                    &mut stream,
                    &GameMessage::RoomRoster {
                        members: vec![
                            RoomMember {
                                name: "host".to_string(),
                                mesh_addr: "127.0.0.1:39394".parse().unwrap(),
                            },
                            RoomMember {
                                name: "guest".to_string(),
                                mesh_addr: "127.0.0.1:39395".parse().unwrap(),
                            },
                        ],
                        your_index,
                    },
                )
                .unwrap();
            });

            let mesh_listener = loopback_listener();
            let err = join_room_as_guest(room_addr, "guest", &mesh_listener).unwrap_err();
            fake_host.join().unwrap();

            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidData,
                "your_index={your_index}は受け付けないはず"
            );
        }
    }

    #[test]
    fn the_host_rejects_guest_lists_that_do_not_line_up() {
        // 接続・名前・メッシュ宛先は同じ順の同数でなければインデックスが対応しない。
        let host_mesh_listener = loopback_listener();

        let err = start_room_as_host(
            &mut [],
            &["guest".to_string()],
            &[],
            "host",
            &host_mesh_listener,
            test_config(),
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn an_unspecified_host_mesh_address_is_resolved_through_the_address_the_guest_dialed() {
        let dialed: SocketAddr = "192.168.1.10:39394".parse().unwrap();

        assert_eq!(
            resolve_host_mesh_addr("0.0.0.0:40000".parse().unwrap(), dialed),
            "192.168.1.10:40000".parse().unwrap(),
            "0.0.0.0で待ち受けている主催者へは、実際に繋がったIPで接続するはず"
        );
        assert_eq!(
            resolve_host_mesh_addr("10.0.0.5:40000".parse().unwrap(), dialed),
            "10.0.0.5:40000".parse().unwrap(),
            "具体的なIPを広告している場合はそのまま使うはず"
        );
    }

    #[test]
    fn a_full_mesh_of_one_participant_needs_no_connection() {
        // 主催者だけのルーム(ゲスト0人)では接続は1本も要らない。
        let mesh_listener = loopback_listener();
        let members = vec![RoomMember {
            name: "host".to_string(),
            mesh_addr: mesh_listener.local_addr().unwrap(),
        }];

        let streams = establish_full_mesh(&members, 0, "host", &mesh_listener).unwrap();

        assert!(streams.is_empty());
    }
}
