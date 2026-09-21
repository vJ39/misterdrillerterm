//! N人対戦のルーム参加フローとフルメッシュ確立(#275。docs/spec.md 12.2)。
//!
//! #274のフルメッシュ対戦(`BattleState::from_peer_streams`)が要求する「確立済みの
//! TCP接続N-1本」を用意するところを担う。主催者が参加者を集めて`RoomRoster`・対戦設定・
//! シードを配布し、全員が同じ`members`の並び(room内インデックス)を見て
//! C(n,2)本のメッシュ接続を張る。
//!
//! ロビーUI(ルーム作成・参加者一覧の表示・開始操作)は#276の範囲のため、ここは
//! ブロッキングI/Oで完結する関数群として置く(#276が非ブロッキングな状態機械でラップする)。
//! ルーム参加接続のaccept自体も、参加者が増えるたびに一覧を更新表示するという操作性の
//! ために呼び出し元の責務としている。

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use rand::RngExt;

use crate::net::{self, BattleConfig, GameMessage, RoomMember, TCP_CONNECT_TIMEOUT_MS};

/// 参加者側がルーム参加を終えた結果(自分以外とのメッシュ接続。room内インデックス順,
/// 自分以外の名前を同じ順で並べたもの, 自分のroom内インデックス, ハンドシェイク結果)。
///
/// メッシュ接続が`None`の枠はAI(#300。接続を持たない追加の参加者)。ロビーが別スレッドから
/// チャネルで受け取るため、型に名前を付けておく。
pub type RoomStartResult = (
    Vec<Option<TcpStream>>,
    Vec<String>,
    usize,
    net::HandshakeResult,
);

/// 主催者側。既に`JoinRoom`を受け取った各ゲストとの接続(`guest_room_streams`。
/// `JoinRoom`を受信した順=room内インデックス1,2,3...と対応)へ`RoomRoster`・対戦設定・
/// シードを配布し、フルメッシュのメッシュ接続(自分以外、room内インデックス順)を
/// 確立する。
///
/// `guest_names`/`guest_mesh_addrs`は`guest_room_streams`と同じ順で、それぞれ`JoinRoom`の
/// `name`と、「接続元のIP+`JoinRoom`の`mesh_port`」を呼び出し元が組み立てたもの。
///
/// `ai_count`はこのルームへ追加するAI(#300)の人数。ゲストの後ろへ`mesh_addr`が`None`の
/// `RoomMember`として並べ、ホストがローカルで操作して入力・妨害岩・結果を代理送信する。
///
/// 戻り値の`HandshakeResult`は2人版と同じ型だが、`opponent_name`はN人版では意味を持た
/// ないため空にする(参加者名は`RoomRoster`の`members`が持ち、呼び出し元は自分が組み立てた
/// `guest_names`をそのまま使える)。
pub fn start_room_as_host(
    guest_room_streams: &mut [TcpStream],
    guest_names: &[String],
    guest_mesh_addrs: &[SocketAddr],
    ai_count: usize,
    my_name: &str,
    my_mesh_listener: &TcpListener,
    config: BattleConfig,
) -> io::Result<(Vec<Option<TcpStream>>, net::HandshakeResult)> {
    if guest_room_streams.len() != guest_names.len()
        || guest_room_streams.len() != guest_mesh_addrs.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ゲストの接続・名前・メッシュ宛先は同数で同じ順のはず",
        ));
    }

    // 主催者はroom内インデックス0。以降は`JoinRoom`を受け取った順。
    let mut members = Vec::with_capacity(guest_names.len() + 1 + ai_count);
    members.push(RoomMember {
        name: my_name.to_string(),
        mesh_addr: Some(my_mesh_listener.local_addr()?),
    });
    for (name, &mesh_addr) in guest_names.iter().zip(guest_mesh_addrs) {
        members.push(RoomMember {
            name: name.clone(),
            mesh_addr: Some(mesh_addr),
        });
    }
    // #300: AIはゲストの後ろへ。接続先を持たない枠として全員へ同じ並びで配る。
    for n in 1..=ai_count {
        members.push(RoomMember {
            name: ai_member_name(n),
            mesh_addr: None,
        });
    }

    // シードは主催者がOS乱数から単独で決める(2人版`run_host_handshake`と同じ取り決め。
    // spec.md 12.2ステップ3)。
    let seed: u64 = rand::rng().random();
    // #319: AIは枠ごとに別のシードにする(同じ盤面だと決定論的なオートプレイの結果が
    // ほぼ同じになる)。ゲストもホストが動かすAIと同じ盤面のコピーを持つ必要があるため、
    // 主催者が生成して全員へ配る。
    let ai_seeds: Vec<u64> = (0..ai_count).map(|_| rand::rng().random()).collect();

    for (guest_index, stream) in guest_room_streams.iter_mut().enumerate() {
        // `your_index`だけ送り先ごとに変える(名前が重複していても各参加者が自分を
        // 一意に特定できるようにするため)。
        net::write_message(
            stream,
            &GameMessage::RoomRoster {
                members: members.clone(),
                your_index: guest_index + 1,
            },
        )?;
        net::write_message(stream, &GameMessage::StartConfig(Box::new(config)))?;
        net::write_message(
            stream,
            &GameMessage::SeedAgree {
                seed,
                ai_seeds: ai_seeds.clone(),
            },
        )?;
    }

    let streams = establish_full_mesh(&members, 0, my_name, my_mesh_listener)?;

    Ok((
        streams,
        net::HandshakeResult {
            opponent_name: String::new(),
            config,
            seed,
            ai_seeds,
        },
    ))
}

/// 参加者側。主催者へ接続して`JoinRoom`を送り、`RoomRoster`以降を受け取ってから
/// フルメッシュを確立する。`connect_and_join_room`と`await_room_start`を
/// 順に呼ぶだけの薄い関数(#276。ロビーUIは開始を待つ区間だけ別スレッド化するため、
/// この2関数を分けて個別に呼ぶ)。
///
/// 戻り値は`RoomStartResult`。
/// `HandshakeResult::opponent_name`は主催者側と同じ理由で空にする(名前は2つ目の戻り値が持つ)。
#[allow(dead_code)] // ロビーは2つに分けて呼ぶため、この薄い関数はテストからのみ使う。
pub fn join_room_as_guest(
    host_addr: SocketAddr,
    my_name: &str,
    my_mesh_listener: &TcpListener,
) -> io::Result<RoomStartResult> {
    let room_stream = connect_and_join_room(host_addr, my_name, my_mesh_listener)?;
    await_room_start(room_stream, my_name, my_mesh_listener)
}

/// 参加者側の前半。主催者へ接続して`JoinRoom`を送るだけの、即座に完了する処理
/// (#276)。開始(`RoomRoster`以降)を待つ`await_room_start`は主催者がいつ開始するか
/// 分からず無期限に待つ処理のため、ロビーUIはこちらだけをメインループでブロッキング
/// 呼び出しし、`await_room_start`は別スレッドに載せる。
pub fn connect_and_join_room(
    host_addr: SocketAddr,
    my_name: &str,
    my_mesh_listener: &TcpListener,
) -> io::Result<TcpStream> {
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
    Ok(room_stream)
}

/// 参加者側の後半。`connect_and_join_room`が返した接続で`RoomRoster`以降を受け取り、
/// フルメッシュを確立する(#276)。主催者の開始操作を待つため無期限にブロックし得る。
pub fn await_room_start(
    mut room_stream: TcpStream,
    my_name: &str,
    my_mesh_listener: &TcpListener,
) -> io::Result<RoomStartResult> {
    let host_addr = room_stream.peer_addr()?;
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
    // #300: `members[0]`は常に主催者(人間)なので`mesh_addr`は必ず`Some`。AIの枠は
    // ゲストの後ろにしか現れないため、ここが`None`ならrosterが壊れている。
    let Some(host_mesh_addr) = members[0].mesh_addr else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "members[0](主催者)にメッシュ接続の宛先が無い",
        ));
    };
    members[0].mesh_addr = Some(resolve_host_mesh_addr(host_mesh_addr, host_addr));

    let config = match net::read_message(&mut room_stream)? {
        GameMessage::StartConfig(config) => *config,
        other => return Err(net::unexpected_message("StartConfig", &other)),
    };
    let (seed, ai_seeds) = match net::read_message(&mut room_stream)? {
        GameMessage::SeedAgree { seed, ai_seeds } => (seed, ai_seeds),
        other => return Err(net::unexpected_message("SeedAgree", &other)),
    };

    let streams = establish_full_mesh(&members, my_index, my_name, my_mesh_listener)?;
    let other_names = members
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != my_index)
        .map(|(_, member)| member.name.clone())
        .collect();

    // ルーム参加接続はここで用済み。メッシュ確立まで開いたままにしておき、
    // この関数を抜けるところで閉じる。
    drop(room_stream);

    Ok((
        streams,
        other_names,
        my_index,
        net::HandshakeResult {
            opponent_name: String::new(),
            config,
            seed,
            ai_seeds,
        },
    ))
}

/// `members`(room内インデックス順、自分を含む)と自分のインデックス`my_index`から、
/// 自分以外の全員とのメッシュ接続を確立する(#275)。戻り値は`members`から自分を
/// 除いた順(`BattleState::from_peer_streams`が要求する、`games[1..]`と対応する順)。
///
/// 役割は「room内インデックスが小さい方がTCPサーバ役」という決定的な規則で決まるため、
/// 全員が同じ`members`を見ていれば通信なしに一致する。
///
/// #300: AI(`mesh_addr`が`None`)の枠は接続を張らず、戻り値の同じ位置に`None`を置く。
fn establish_full_mesh(
    members: &[RoomMember],
    my_index: usize,
    my_name: &str,
    mesh_listener: &TcpListener,
) -> io::Result<Vec<Option<TcpStream>>> {
    // この関数はブロッキングI/Oで完結する前提。ロビーのlistener(`lobby.rs`)は
    // 非ブロッキングに設定されているため、accept前にブロッキングへ戻す。
    mesh_listener.set_nonblocking(false)?;

    let mut connections: Vec<Option<TcpStream>> = members.iter().map(|_| None).collect();

    // 自分より小さいインデックスへは自分から繋ぐ(クライアント役)。相手は自分が選んで
    // いるのでインデックスは既知で、`Hello`は名前の確認(繋ぎ先の取り違え検出)に使う。
    for (index, member) in members.iter().enumerate().take(my_index) {
        // #300: AI(`mesh_addr`が`None`)は実際のTCP接続を持たないため繋がない。
        let Some(mesh_addr) = member.mesh_addr else {
            continue;
        };
        let mut stream =
            TcpStream::connect_timeout(&mesh_addr, Duration::from_millis(TCP_CONNECT_TIMEOUT_MS))?;
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
    // peerの特定は名前しか手がかりが無いという設計上の制約(#275)。
    //
    // #300: AI(`mesh_addr`が`None`)は繋いでこないため、受け入れる本数から除く。
    let incoming_count = members
        .iter()
        .skip(my_index + 1)
        .filter(|member| member.mesh_addr.is_some())
        .count();
    for _ in 0..incoming_count {
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
            // #300: AIの枠は名前が一致しても接続の受け入れ先にはならない。
            .find(|(index, member)| {
                member.mesh_addr.is_some()
                    && member.name == peer_name
                    && connections[*index].is_none()
            })
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
        // #300: AIの枠は接続を持たないまま`None`で並びに残す(対戦側が`games[1..]`の
        // どの位置がAIかを、この並びから知る)。人間の枠が空なら従来通りエラー。
        if members[index].mesh_addr.is_none() {
            streams.push(None);
            continue;
        }
        streams.push(Some(connection.ok_or_else(|| {
            io::Error::other(format!(
                "room内インデックス{index}との接続が確立できていない"
            ))
        })?));
    }
    Ok(streams)
}

/// ルームへ追加したAI(#300)の表示名。`number`は1始まり。
///
/// rosterを組む側(この`room`)と、対戦の参加者名を組む側(`lobby`)の両方で使うため関数に
/// しておく(名前がずれると、ホストの参加者一覧と他の参加者が見るrosterが食い違う)。
pub fn ai_member_name(number: usize) -> String {
    format!("AI {number}")
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
/// 仕様(docs/spec.md 12.2)に書いていない実装側の補正。主催者は自分の`mesh_addr`を
/// `TcpListener::local_addr()`から作るが、ロビーのlistenerは全インターフェース(0.0.0.0)に
/// bindされるため、そのIPのままでは参加者が主催者へ繋げない。参加者はルーム参加接続で使った宛先で主催者に到達できることが確かなので、
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
    use crate::game::board::Cell;
    use crate::game::player::Direction;
    use crate::game::{Game, InputAction};
    use crate::settings::Settings;
    use std::net::Ipv4Addr;
    use std::thread;

    /// テストのポンプ回数の上限。ループバックの配送待ちで何周か空回りするため、必要な
    /// フレーム数より多めに取る(`battle.rs`のテストと同じ考え方)。
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
        /// 自分以外とのメッシュ接続(room内インデックス順)。`None`はAIの枠(#300)。
        streams: Vec<Option<TcpStream>>,
        /// 自分以外の名前(`streams`と同じ順)。
        other_names: Vec<String>,
        handshake: net::HandshakeResult,
    }

    /// `names`(index 0が主催者)の人数でルームを1つ成立させ、room内インデックス順の
    /// 参加結果を返す。
    fn run_room(names: &[String]) -> Vec<Participant> {
        run_room_with_ai(names, 0)
    }

    /// `run_room`のAIあり版(#300)。人間`names`の後ろへAIを`ai_count`人追加する。
    ///
    /// ルーム参加接続のacceptは呼び出し元の責務なので、ここがその役を担う。
    /// ゲストのroom内インデックスは主催者が`JoinRoom`を受け取った順で決まるため、
    /// 順序を確定させるためゲストは1人ずつ参加させる(`JoinRoom`送信後は`RoomRoster`待ちで
    /// ブロックするので、次のゲストを起こす前にインデックスが確定する)。
    fn run_room_with_ai(names: &[String], ai_count: usize) -> Vec<Participant> {
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
                let (streams, other_names, _my_index, handshake) =
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
            ai_count,
            &names[0],
            &host_mesh_listener,
            test_config(),
        )
        .unwrap();

        let mut other_names = guest_names;
        other_names.extend((1..=ai_count).map(ai_member_name));
        let mut participants = vec![Participant {
            name: names[0].clone(),
            streams: host_streams,
            other_names,
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
    /// `ai_count`はルームへ追加したAI(#300)の人数。AIは人間の後ろに並ぶため、各参加者の
    /// `streams`の末尾`ai_count`個が接続なしの枠になる。
    fn assert_streams_are_in_room_index_order(participants: &mut [Participant], ai_count: usize) {
        let total = participants.len() + ai_count;
        for (my_index, participant) in participants.iter_mut().enumerate() {
            assert_eq!(
                participant.streams.len(),
                total - 1,
                "参加者{my_index}は自分以外の全員ぶんの枠を持つはず"
            );
            for (stream_index, slot) in participant.streams.iter().enumerate() {
                assert_eq!(
                    slot.is_none(),
                    stream_index >= total - 1 - ai_count,
                    "参加者{my_index}のstreams[{stream_index}]: AIの枠(#300)だけが接続なしのはず"
                );
            }
            for stream in participant.streams.iter_mut().flatten() {
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
            for (stream_index, slot) in participant.streams.iter_mut().enumerate() {
                let Some(stream) = slot else {
                    continue;
                };
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

        assert_streams_are_in_room_index_order(&mut participants, 0);
    }

    #[test]
    fn a_four_player_room_connects_everyone_in_room_index_order() {
        let mut participants = run_room(&numbered_names(4));

        assert_streams_are_in_room_index_order(&mut participants, 0);
    }

    #[test]
    fn a_two_player_room_goes_through_the_same_flow_with_a_single_connection() {
        // 2人でも同じ経路で成立する(主催者が接続を受け、参加者が繋ぐ側になる)。
        let mut participants = run_room(&numbered_names(2));

        assert_streams_are_in_room_index_order(&mut participants, 0);
    }

    #[test]
    fn everyone_in_the_room_ends_up_with_the_same_config_and_seed() {
        let participants = run_room(&numbered_names(4));

        let host = &participants[0].handshake;
        assert_eq!(host.config, test_config(), "主催者は自分の設定を使うはず");
        for (index, participant) in participants.iter().enumerate().skip(1) {
            assert_eq!(
                participant.handshake.config, host.config,
                "参加者{index}は主催者の設定に従うはず"
            );
            assert_eq!(participant.handshake.seed, host.seed);
            assert_eq!(participant.handshake.ai_seeds, host.ai_seeds);
        }
        assert!(
            host.ai_seeds.is_empty(),
            "AIがいないルームではAIぶんのシード(#319)は無いはず"
        );
    }

    #[test]
    fn everyone_in_the_room_gets_the_same_separate_seed_for_each_ai() {
        // #319: AIの枠ごとに別のシードを主催者が決め、全員へ同じ並びで配る。ゲストが持つAIの
        // 盤面コピーは主催者が動かしている盤面と一致していないと、表示や順位が食い違う。
        const AI_COUNT: usize = 2;
        let participants = run_room_with_ai(&numbered_names(2), AI_COUNT);

        let host = &participants[0].handshake;
        assert_eq!(
            host.ai_seeds.len(),
            AI_COUNT,
            "AIの枠と同数のシードを配るはず(対戦側はこの並びをAIの順で引く)"
        );
        assert_ne!(
            host.ai_seeds[0], host.ai_seeds[1],
            "AIどうしで別のシードになるはず"
        );
        assert!(
            !host.ai_seeds.contains(&host.seed),
            "人間が共有するシードとも別になるはず"
        );
        for (index, participant) in participants.iter().enumerate().skip(1) {
            assert_eq!(
                participant.handshake.ai_seeds, host.ai_seeds,
                "参加者{index}は主催者が配ったAIぶんのシードをそのまま受け取るはず"
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

        assert_streams_are_in_room_index_order(&mut participants, 0);
        assert_eq!(participants[1].other_names, vec!["dup", "solo"]);
        assert_eq!(participants[2].other_names, vec!["dup", "dup"]);
    }

    #[test]
    fn the_established_mesh_drives_a_four_player_battle() {
        // 確立した接続をそのまま`BattleState::from_peer_streams`へ渡し、全員の操作が
        // 他の全員の手元へ届くことを見る(#274との接続部分の確認)。#299で盤面の
        // 突き合わせをやめたため、tick数や盤面の一致ではなく操作の反映で確認する。
        const N: usize = 4;
        let participants = run_room(&numbered_names(N));
        let seed = participants[0].handshake.seed;
        let config = participants[0].handshake.config;

        let mut states: Vec<BattleState> = participants
            .into_iter()
            .enumerate()
            .map(|(my_index, participant)| {
                let games: Vec<Game> = (0..N)
                    .map(|_| new_game_from_battle_config(seed, &config))
                    .collect();
                let mut player_names = vec![participant.name];
                player_names.extend(participant.other_names);
                BattleState::from_peer_streams(games, player_names, participant.streams, my_index)
                    .unwrap()
            })
            .collect();

        // 横移動は足場が無いと受け付けられないため、全員ぶんの盤面に同じ足場を置き、
        // 左右を空けておく(全員が同じシードから作るため、同じ変更で同じ盤面になる)。
        for state in states.iter_mut() {
            for game in state.games.iter_mut() {
                let (row, col) = (game.player.row, game.player.col);
                game.board.rows[row + 1][col] = Cell::Rock { hits: 0 };
                game.board.rows[row][col - 1] = Cell::Empty;
                game.board.rows[row][col + 1] = Cell::Empty;
            }
        }

        // 参加者ごとに別の向きになる操作を割り当てる。接続の順序が食い違って他人の入力を
        // 取り違えていれば、向きの並びがずれてここで壊れる。
        let action_for = |index: usize| match index {
            0 => InputAction::MoveLeft,
            1 => InputAction::MoveRight,
            2 => InputAction::FaceUp,
            _ => InputAction::FaceDown,
        };
        let facing_for = |index: usize| match index {
            0 => Direction::Left,
            1 => Direction::Right,
            2 => Direction::Up,
            _ => Direction::Down,
        };

        for (my_index, state) in states.iter_mut().enumerate() {
            state.advance(Duration::ZERO, Some(action_for(my_index)));
        }

        for _ in 0..MAX_PUMPS {
            for state in states.iter_mut() {
                // 盤面を進めずに受信だけ回す(配送待ちで酸素を消費させない)。
                state.advance(Duration::ZERO, None);
            }
            let all_arrived = states.iter().enumerate().all(|(my_index, state)| {
                (0..N).all(|participant| {
                    state.games[games_index_of(my_index, participant)]
                        .player
                        .facing
                        == facing_for(participant)
                })
            });
            if all_arrived {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        for (my_index, state) in states.iter().enumerate() {
            assert_eq!(
                state.outcome(),
                None,
                "参加者{my_index}: この範囲では決着しないはず"
            );
            for participant in 0..N {
                assert_eq!(
                    state.games[games_index_of(my_index, participant)]
                        .player
                        .facing,
                    facing_for(participant),
                    "参加者{participant}の操作が参加者{my_index}の手元へ届いていない"
                );
            }
        }
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
                                mesh_addr: Some("127.0.0.1:39394".parse().unwrap()),
                            },
                            RoomMember {
                                name: "guest".to_string(),
                                mesh_addr: Some("127.0.0.1:39395".parse().unwrap()),
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
            0,
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
            mesh_addr: Some(mesh_listener.local_addr().unwrap()),
        }];

        let streams = establish_full_mesh(&members, 0, "host", &mesh_listener).unwrap();

        assert!(streams.is_empty());
    }

    // -----------------------------------------------------------------------
    // ルームへ追加したAI(#300)。AIはTCP接続を持たない枠として`members`に並ぶため、
    // メッシュ確立では「繋ぎに行かない・受け入れ本数に数えない・並びには残す」となる。
    // -----------------------------------------------------------------------

    /// メッシュ接続のクライアント役。`addr`へ繋いで`Hello`を送り、相手の`Hello`を待つ
    /// (`establish_full_mesh`が自分より小さいインデックスへ行う手順と同じ)。
    fn spawn_mesh_client(addr: SocketAddr, name: String) -> thread::JoinHandle<TcpStream> {
        thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            net::write_message(&mut stream, &GameMessage::Hello { name }).unwrap();
            read_hello(&mut stream).unwrap();
            stream
        })
    }

    /// メッシュ接続のサーバ役。1件受け入れて`Hello`を受け取り、自分の`Hello`を返す
    /// (`establish_full_mesh`が自分より大きいインデックスへ行う手順と同じ)。
    fn spawn_mesh_server(listener: TcpListener, name: String) -> thread::JoinHandle<TcpStream> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_hello(&mut stream).unwrap();
            net::write_message(&mut stream, &GameMessage::Hello { name }).unwrap();
            stream
        })
    }

    #[test]
    fn a_full_mesh_leaves_the_ai_slot_empty_and_connects_only_the_humans() {
        // 主催者の視点。ゲスト1人+AI1人なら、受け入れるのは人間の1本だけ。AIを受け入れ
        // 本数に数えると、来ない接続をacceptし続けて対戦が始まらなくなる。
        let my_listener = loopback_listener();
        let my_addr = my_listener.local_addr().unwrap();
        let guest_listener = loopback_listener();
        let members = vec![
            RoomMember {
                name: "host".to_string(),
                mesh_addr: Some(my_addr),
            },
            RoomMember {
                name: "guest".to_string(),
                mesh_addr: Some(guest_listener.local_addr().unwrap()),
            },
            RoomMember {
                name: ai_member_name(1),
                mesh_addr: None,
            },
        ];
        let guest = spawn_mesh_client(my_addr, "guest".to_string());

        let streams = establish_full_mesh(&members, 0, "host", &my_listener).unwrap();
        let _guest_side = guest.join().unwrap();

        assert_eq!(streams.len(), 2, "自分以外の全員ぶんの枠が並ぶはず");
        assert!(streams[0].is_some(), "人間のゲストとは接続を張るはず");
        assert!(
            streams[1].is_none(),
            "AIの枠は接続なしのまま並びに残るはず(対戦側がこの位置でAIを見分ける)"
        );
    }

    #[test]
    fn a_full_mesh_never_dials_an_ai_member() {
        // 参加者の視点。自分より小さいインデックスへは自分から繋ぐが、AIの枠は宛先を
        // 持たないため繋ぎに行かず、並びの位置だけ空けて残す。
        // (実際のrosterではAIは最後に並ぶ。ここは繋ぎに行かない分岐そのものの確認。)
        let my_listener = loopback_listener();
        let host_listener = loopback_listener();
        let host_addr = host_listener.local_addr().unwrap();
        let members = vec![
            RoomMember {
                name: "host".to_string(),
                mesh_addr: Some(host_addr),
            },
            RoomMember {
                name: ai_member_name(1),
                mesh_addr: None,
            },
            RoomMember {
                name: "me".to_string(),
                mesh_addr: Some(my_listener.local_addr().unwrap()),
            },
        ];
        let host = spawn_mesh_server(host_listener, "host".to_string());

        let streams = establish_full_mesh(&members, 2, "me", &my_listener).unwrap();
        let _host_side = host.join().unwrap();

        assert_eq!(streams.len(), 2, "自分以外の全員ぶんの枠が並ぶはず");
        assert!(streams[0].is_some(), "主催者とは接続を張るはず");
        assert!(streams[1].is_none(), "AIの枠へは繋ぎに行かないはず");
    }

    #[test]
    fn a_room_with_an_ai_still_connects_the_humans_in_room_index_order() {
        // 人間3人+AI1人。AIが混ざっても人間どうしの接続の本数・順序は変わらない。
        let mut participants = run_room_with_ai(&numbered_names(3), 1);

        assert_streams_are_in_room_index_order(&mut participants, 1);
    }

    #[test]
    fn everyone_sees_the_added_ai_at_the_end_of_the_roster() {
        // AIは主催者がrosterの末尾へ追加する。ゲストが受け取る`RoomRoster`でも同じ並びに
        // なっていないと、代理送信(#300)のroom内インデックスが指す相手が食い違う。
        const AI_COUNT: usize = 2;
        let names = numbered_names(2);
        let participants = run_room_with_ai(&names, AI_COUNT);

        for (my_index, participant) in participants.iter().enumerate() {
            let mut expected: Vec<String> = names
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != my_index)
                .map(|(_, name)| name.clone())
                .collect();
            expected.extend((1..=AI_COUNT).map(ai_member_name));
            assert_eq!(
                participant.other_names, expected,
                "参加者{my_index}はAIを人間の後ろに並べた名前一覧を受け取るはず"
            );
        }
    }
}
