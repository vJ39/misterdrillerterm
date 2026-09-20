//! 対戦相手を探して招待をやり取りするロビー画面の状態(#256。spec.md 12.1)。
//!
//! UDP探索(`discovery.rs`)の候補リストと、招待→ルーム参加→フルメッシュ確立(`room.rs`)→
//! 対戦開始(`battle.rs`)までの流れを1つの状態機械にまとめる。入力の取り込みと描画は
//! 画面側(`app::screens::tick_network_lobby`・`ui::render::draw_network_lobby`)が行い、
//! ここは「押された操作」と「経過時間」を受け取ってフェーズを進めることに専念する
//! (ターミナルを持たずに結合テストできるようにするため)。
//!
//! N人対戦(#276。docs/multiplayer-4p-lobby-design.md)では役割が2人版から反転していて、
//! **招待した側が常にルームの主催者(TCPサーバ役)**・承諾した側がゲスト(クライアント役)
//! になる。主催者は承諾が返るたびにゲストを1人ずつ迎え入れ、集まったところで開始操作
//! (Tab)を出す。

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::battle::{BattleState, new_game_from_battle_config};
use crate::discovery::{DiscoveredPeer, Discovery};
use crate::game::InputAction;
use crate::net::{
    self, BattleConfig, DEFAULT_TCP_PORT, DiscoveryPacket, GameMessage, INVITE_TIMEOUT_MS,
    PacketType, TCP_CONNECT_TIMEOUT_MS,
};
use crate::room;

/// 短い通知(拒否・タイムアウト・接続失敗)を表示し続ける時間(ms)。この後は自動的に
/// 探索へ戻る。
const NOTICE_DISPLAY_MS: u64 = 1500;

/// 対戦用TCPポートの空きを探す個数。既定値から1つずつ上へ試す(spec.md 12.1
/// 「使用中なら39395, 39396…」)。
const TCP_PORT_SEARCH_COUNT: u16 = 16;

/// 接続に失敗したときの通知文。主催者側・ゲスト側の双方で使う。
const CONNECT_FAILED_MESSAGE: &str = "接続できませんでした";

/// ルーム1つに集まれる最大人数(主催者自身を含む。対戦人数の上限は4人)。
const ROOM_MAX_PLAYERS: usize = 4;

/// `room::await_room_start`の結果(自分以外とのメッシュ接続, 自分以外の名前, ハンドシェイク)。
/// 別スレッドからチャネルで受け取るため型に名前を付ける。
pub type RoomStartResult = (Vec<TcpStream>, Vec<String>, net::HandshakeResult);

/// 主催者が既に迎え入れたゲスト1人ぶん(設計書2節)。
///
/// 設計書では非公開structだが、公開enum`LobbyPhase`のフィールドに出てくるため
/// `private_interfaces`(`-D warnings`で失敗する)を避けて公開にし、中身は非公開の
/// まま表示用の`name()`だけ見せる。
pub struct HostedGuest {
    /// ルーム参加用の接続。開始時に`RoomRoster`以降をここへ流す。
    room_stream: TcpStream,
    name: String,
    /// このゲストのメッシュ接続の待ち受け先(参加接続の接続元IP+申告されたポート)。
    mesh_addr: SocketAddr,
}

impl HostedGuest {
    /// 参加者一覧の表示用。
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// ロビーの状態。
pub struct LobbyState {
    discovery: Discovery,
    /// ルーム参加用のTCP listener。ロビーにいる間はずっとlistenしておき、このポートを
    /// HELLOで広告し続ける(招待を受けた時点で改めてbindすると、広告済みのポートが
    /// 空いている保証が無い)。非ブロッキングに設定済み。
    listener: TcpListener,
    /// メッシュ接続用のTCP listener(設計書4節)。ルーム参加の接続とメッシュの接続を
    /// 同じポートで受けると区別できないため、別ポートで待ち受ける。
    mesh_listener: TcpListener,
    /// 自分の表示名。HELLOの広告にも、ルーム参加の`JoinRoom`にも使う。
    my_name: String,
    /// 候補リスト上のカーソル位置。
    selection: usize,
    phase: LobbyPhase,
}

/// ロビーの進行段階。
pub enum LobbyPhase {
    /// 候補を探しながら招待を待っている通常状態。`guests`が空でなければ、既にルームを
    /// 開いていて追加の招待を送れる状態(設計書2節)。
    Discovering { guests: Vec<HostedGuest> },
    /// 自分から招待を送り、相手の応答を待っている。
    AwaitingInviteResponse {
        target: DiscoveredPeer,
        sent_at: Instant,
        guests: Vec<HostedGuest>,
    },
    /// 招待を受け取り、承諾するか聞いている。
    IncomingInvite { from: DiscoveredPeer },
    /// ACCEPTを受けたゲストのTCP接続を待っている(主催者側)。
    AcceptingGuestConnection {
        guest_name: String,
        started: Instant,
        guests: Vec<HostedGuest>,
    },
    /// 招待を承諾し、主催者への接続を試みている(ゲスト側)。
    ConnectingToHost { addr: SocketAddr, host_name: String },
    /// 主催者へ接続・`JoinRoom`送信済みで、開始(`RoomRoster`以降)を別スレッドで待っている。
    WaitingForRoomStart {
        result_rx: mpsc::Receiver<io::Result<RoomStartResult>>,
    },
    /// 拒否・タイムアウト・接続失敗を短く伝える。時間が経つと探索へ戻る。
    ///
    /// `guests`は主催者側で既に迎え入れていたゲスト(#284。招待や接続の失敗1つで
    /// ルーム全体を解散させないよう、通知を挟んでも持ち越す)。ゲスト側の失敗
    /// (このロビー自身がまだ誰も迎えていない)では空になる。
    Notice {
        message: String,
        shown_at: Instant,
        guests: Vec<HostedGuest>,
    },
}

/// ロビーの1フレームの結果。
pub enum LobbyOutcome {
    /// ロビーに留まる。
    Stay,
    /// タイトルへ戻る。
    Leave,
    /// 対戦が成立した。
    Battle(Box<BattleState>),
}

/// `apply_packets`が決めた、パケット1つぶんの反映内容。`HostedGuest`が`TcpStream`を
/// 持ちcloneできないため、「現在のフェーズを見て決める」ところと「フェーズを入れ替える」
/// ところを分ける(借用が重ならないようにするため)。
enum PacketEffect {
    /// 招待された。
    IncomingInvite(DiscoveredPeer),
    /// ルームを開いている最中に招待された。返信だけして状態は変えない。
    DeclineWhileHosting(DiscoveredPeer),
    /// 招待が承諾された。ゲストの接続待ちへ移る。
    InviteAccepted { guest_name: String },
    /// 招待が断られた。
    InviteDeclined,
}

impl LobbyState {
    /// 探索用UDPソケットと対戦用TCP listener 2本を確保してロビーを開始する。
    /// どれかのポートが取れない場合はエラーを返す(呼び出し元はタイトルへ留まる)。
    pub fn new(my_name: String) -> io::Result<Self> {
        let listener = bind_battle_listener()?;
        listener.set_nonblocking(true)?;
        let tcp_port = listener.local_addr()?.port();
        // メッシュ用は`bind_battle_listener`をもう一度呼ぶだけでよい(1本目のポートは
        // 使用中になっているため、自然に別の空きポートが取れる。設計書4節)。
        let mesh_listener = bind_battle_listener()?;
        let discovery = Discovery::start(my_name.clone(), tcp_port)?;
        Ok(Self::new_with(discovery, listener, mesh_listener, my_name))
    }

    fn new_with(
        discovery: Discovery,
        listener: TcpListener,
        mesh_listener: TcpListener,
        my_name: String,
    ) -> Self {
        Self {
            discovery,
            listener,
            mesh_listener,
            my_name,
            selection: 0,
            phase: discovering(),
        }
    }

    pub fn phase(&self) -> &LobbyPhase {
        &self.phase
    }

    pub fn peers(&self) -> &[DiscoveredPeer] {
        self.discovery.peers()
    }

    pub fn selection(&self) -> usize {
        self.selection
    }

    pub fn my_name(&self) -> &str {
        &self.my_name
    }

    /// 既にルームへ迎え入れたゲスト(参加者一覧の表示用)。ゲストを持たないフェーズでは空。
    pub fn hosted_guests(&self) -> &[HostedGuest] {
        match &self.phase {
            LobbyPhase::Discovering { guests }
            | LobbyPhase::AwaitingInviteResponse { guests, .. }
            | LobbyPhase::AcceptingGuestConnection { guests, .. } => guests,
            _ => &[],
        }
    }

    /// 1フレーム分進める。`actions`はこのフレームに届いた操作、`config`は自分が
    /// 主催者になった場合に参加者へ強制適用する設定(spec.md 12.2)。
    pub fn update(&mut self, actions: &[InputAction], config: BattleConfig) -> LobbyOutcome {
        let packets = self.discovery.tick();
        self.apply_packets(packets);
        self.clamp_selection();

        for &action in actions {
            if let Some(outcome) = self.apply_action(action, config) {
                return outcome;
            }
        }

        self.advance_phase()
    }

    /// 受信した自分宛のINVITE/ACCEPT/DECLINEを、現在のフェーズに応じて反映する。
    fn apply_packets(&mut self, packets: Vec<DiscoveryPacket>) {
        for packet in packets {
            match self.packet_effect(&packet) {
                Some(PacketEffect::IncomingInvite(from)) => {
                    self.phase = LobbyPhase::IncomingInvite { from };
                }
                Some(PacketEffect::DeclineWhileHosting(from)) => {
                    // 設計書に無い判断(#276の報告に記載): ルームを開いている最中の招待は
                    // 自動で断る。承諾してしまうと迎え入れ済みのゲストの接続を捨てる=
                    // 相手を黙って切断することになるため。
                    let _ = self.discovery.send_decline(&from);
                }
                Some(PacketEffect::InviteAccepted { guest_name }) => {
                    let guests = self.take_guests();
                    self.phase = LobbyPhase::AcceptingGuestConnection {
                        guest_name,
                        started: Instant::now(),
                        guests,
                    };
                }
                Some(PacketEffect::InviteDeclined) => {
                    let guests = self.take_guests();
                    self.phase = notice("相手に断られました", guests);
                }
                None => {}
            }
        }
    }

    /// パケット1つを現在のフェーズと突き合わせ、何をするか決める(状態は変えない)。
    fn packet_effect(&self, packet: &DiscoveryPacket) -> Option<PacketEffect> {
        match (&self.phase, packet.packet_type) {
            // 招待された。送り主は候補リストに載っているはず(HELLOを1秒間隔で
            // 流し合っているため)で、載っていなければ返信先が分からないので無視する。
            (LobbyPhase::Discovering { guests }, PacketType::Invite) => {
                let from = self
                    .discovery
                    .peers()
                    .iter()
                    .find(|peer| peer.sender_id == packet.sender_id)?
                    .clone();
                if guests.is_empty() {
                    Some(PacketEffect::IncomingInvite(from))
                } else {
                    Some(PacketEffect::DeclineWhileHosting(from))
                }
            }
            (LobbyPhase::AwaitingInviteResponse { target, .. }, PacketType::Accept)
                if target.sender_id == packet.sender_id =>
            {
                // 招待した側が主催者になるため、接続先(相手のポート)は使わない。
                // ゲストの方が広告済みのポートへ接続してくる(設計書1節)。
                Some(PacketEffect::InviteAccepted {
                    guest_name: target.player_name.clone(),
                })
            }
            (LobbyPhase::AwaitingInviteResponse { target, .. }, PacketType::Decline)
                if target.sender_id == packet.sender_id =>
            {
                Some(PacketEffect::InviteDeclined)
            }
            _ => None,
        }
    }

    /// 操作を1つ反映する。ロビーを抜ける・対戦が成立する場合だけ`Some`を返す。
    fn apply_action(&mut self, action: InputAction, config: BattleConfig) -> Option<LobbyOutcome> {
        // `guests`を借りたままだと自分自身を変更できないため、必要な情報だけ先に取る。
        let has_guests = !self.hosted_guests().is_empty();

        match &self.phase {
            LobbyPhase::Discovering { .. } => match action {
                InputAction::FaceUp => self.move_selection(false),
                InputAction::FaceDown => self.move_selection(true),
                InputAction::Confirm => self.invite_selected(),
                // Tab=ルーム開始。ゲストが1人以上いるときだけ意味を持つ(設計書3節)。
                InputAction::StartRoom => {
                    if has_guests {
                        return Some(self.start_room(config));
                    }
                }
                InputAction::Quit => {
                    // 相手の候補リストから即座に消えるよう、抜ける前にBYEを流す。
                    self.discovery.send_bye();
                    return Some(LobbyOutcome::Leave);
                }
                _ => {}
            },
            // 招待のキャンセル。探索自体は続けるためBYEは送らない。迎え入れ済みの
            // ゲストは持ち越す。
            LobbyPhase::AwaitingInviteResponse { .. } => {
                if action == InputAction::Quit {
                    let guests = self.take_guests();
                    self.phase = LobbyPhase::Discovering { guests };
                }
            }
            LobbyPhase::IncomingInvite { from } => match action {
                // Enter=承諾、Esc=拒否(既存の入力体系に合わせる)。
                InputAction::Confirm => {
                    let host = from.clone();
                    if self.discovery.send_accept(&host).is_err() {
                        self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new());
                        return None;
                    }
                    // 承諾した側はゲスト(TCPクライアント役)になる(設計書1節)。
                    // ルームに入るので自分の募集はここで止める(主催者側は追加の招待を
                    // 送るため広告を続ける。止めるのは開始する時)。
                    self.discovery.send_bye();
                    self.phase = LobbyPhase::ConnectingToHost {
                        // 接続先は主催者のIPと、HELLOで広告されていたTCPポート。
                        addr: SocketAddr::new(host.addr, host.tcp_port),
                        host_name: host.player_name,
                    };
                }
                InputAction::Quit => {
                    let inviter = from.clone();
                    let _ = self.discovery.send_decline(&inviter);
                    self.phase = discovering();
                }
                _ => {}
            },
            // 接続中・開始待ち・通知表示中は操作を受け付けない。
            LobbyPhase::AcceptingGuestConnection { .. }
            | LobbyPhase::ConnectingToHost { .. }
            | LobbyPhase::WaitingForRoomStart { .. }
            | LobbyPhase::Notice { .. } => {}
        }

        None
    }

    /// 時間経過・接続の進行によるフェーズの更新。ルーム開始(=`config`を使う経路)は
    /// Tabキーの操作から呼ぶため、ここでは設定を受け取らない。
    fn advance_phase(&mut self) -> LobbyOutcome {
        match &self.phase {
            // 招待を受けた側には期限を設けない(招待した側が`INVITE_TIMEOUT_MS`で
            // 諦めるため、両側に時計を持たせる必要は無い)。
            LobbyPhase::Discovering { .. } | LobbyPhase::IncomingInvite { .. } => {
                LobbyOutcome::Stay
            }
            LobbyPhase::AwaitingInviteResponse { sent_at, .. } => {
                let sent_at = *sent_at;
                if sent_at.elapsed() >= Duration::from_millis(INVITE_TIMEOUT_MS) {
                    let guests = self.take_guests();
                    self.phase = notice("応答がありませんでした", guests);
                }
                LobbyOutcome::Stay
            }
            LobbyPhase::AcceptingGuestConnection { started, .. } => {
                let started = *started;
                self.accept_guest(started);
                LobbyOutcome::Stay
            }
            LobbyPhase::ConnectingToHost { addr, .. } => {
                let addr = *addr;
                self.connect_to_host(addr)
            }
            LobbyPhase::WaitingForRoomStart { .. } => self.receive_room_start(),
            LobbyPhase::Notice { shown_at, .. } => {
                let shown_at = *shown_at;
                if shown_at.elapsed() >= Duration::from_millis(NOTICE_DISPLAY_MS) {
                    let guests = self.take_guests();
                    self.phase = LobbyPhase::Discovering { guests };
                }
                LobbyOutcome::Stay
            }
        }
    }

    /// 主催者として、ACCEPTを返したゲストのルーム参加接続を受け入れる(非ブロッキング
    /// のため毎フレーム1回試す)。`JoinRoom`の読み取りだけは短時間のブロッキングで
    /// 済ませる(設計書4節)。
    fn accept_guest(&mut self, started: Instant) {
        match self.listener.accept() {
            Ok((stream, peer_addr)) => match read_join_room(stream, peer_addr) {
                Ok(guest) => {
                    let mut guests = self.take_guests();
                    guests.push(guest);
                    self.phase = LobbyPhase::Discovering { guests };
                }
                Err(_) => {
                    // このゲストの接続には失敗したが、既に迎え入れていた他のゲストは
                    // 持ち越す(#284)。
                    let guests = self.take_guests();
                    self.phase = notice(CONNECT_FAILED_MESSAGE, guests);
                }
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= Duration::from_millis(TCP_CONNECT_TIMEOUT_MS) {
                    let guests = self.take_guests();
                    self.phase = notice(CONNECT_FAILED_MESSAGE, guests);
                }
            }
            Err(_) => {
                let guests = self.take_guests();
                self.phase = notice(CONNECT_FAILED_MESSAGE, guests);
            }
        }
    }

    /// 集まったゲストで対戦を開始する(主催者)。`room::start_room_as_host`はメッシュ
    /// 確立まで進むためブロッキングだが、対戦成立までの一度きりの処理として扱う
    /// (設計書4節)。
    fn start_room(&mut self, config: BattleConfig) -> LobbyOutcome {
        // 開始したら募集は終わり(spec.md 12.1)。
        self.discovery.send_bye();

        let guests = self.take_guests();
        let mut guest_room_streams = Vec::with_capacity(guests.len());
        let mut guest_names = Vec::with_capacity(guests.len());
        let mut guest_mesh_addrs = Vec::with_capacity(guests.len());
        for guest in guests {
            guest_room_streams.push(guest.room_stream);
            guest_names.push(guest.name);
            guest_mesh_addrs.push(guest.mesh_addr);
        }

        let started = room::start_room_as_host(
            &mut guest_room_streams,
            &guest_names,
            &guest_mesh_addrs,
            &self.my_name,
            &self.mesh_listener,
            config,
        );
        match started {
            Ok((streams, handshake)) => self.battle_from_room(streams, guest_names, handshake),
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new());
                LobbyOutcome::Stay
            }
        }
    }

    /// ゲストとして主催者へ接続し、`JoinRoom`を送る。接続は`connect_timeout`自体が
    /// 待ち時間を持つため1回で決着させ、その後の「主催者が開始するのを待つ」区間は
    /// いつ終わるか分からないため別スレッドへ載せる(設計書5節)。
    fn connect_to_host(&mut self, addr: SocketAddr) -> LobbyOutcome {
        // メッシュ用listenerは待ち受けスレッドへ渡すため複製する(自分は以降使わないが、
        // ロビーの持ち物として開いたままにしておく)。
        let joined = self.mesh_listener.try_clone().and_then(|mesh_listener| {
            let room_stream = room::connect_and_join_room(addr, &self.my_name, &mesh_listener)?;
            Ok((room_stream, mesh_listener))
        });
        let (room_stream, mesh_listener) = match joined {
            Ok(joined) => joined,
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new());
                return LobbyOutcome::Stay;
            }
        };

        let (result_tx, result_rx) = mpsc::channel();
        let my_name = self.my_name.clone();
        thread::spawn(move || {
            // 受け取り手(ロビー)が先にいなくなることもあるため、送信失敗は無視する。
            let _ = result_tx.send(room::await_room_start(
                room_stream,
                &my_name,
                &mesh_listener,
            ));
        });
        self.phase = LobbyPhase::WaitingForRoomStart { result_rx };
        LobbyOutcome::Stay
    }

    /// 主催者の開始を待っているスレッドから結果を受け取る(まだ届いていなければ待つ)。
    fn receive_room_start(&mut self) -> LobbyOutcome {
        let received = match &self.phase {
            LobbyPhase::WaitingForRoomStart { result_rx } => result_rx.try_recv(),
            _ => return LobbyOutcome::Stay,
        };

        match received {
            Ok(Ok((streams, other_names, handshake))) => {
                self.battle_from_room(streams, other_names, handshake)
            }
            // 待ち受けスレッドが失敗した場合と、結果を送らずに終わった場合。
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new());
                LobbyOutcome::Stay
            }
            Err(mpsc::TryRecvError::Empty) => LobbyOutcome::Stay,
        }
    }

    /// 確立したメッシュ接続と参加者名(自分以外、room内インデックス順)から対戦状態を
    /// 組み立てる。盤面は全員が同じシード・同じ設定で作る(spec.md 12.2)。
    fn battle_from_room(
        &mut self,
        streams: Vec<TcpStream>,
        other_names: Vec<String>,
        handshake: net::HandshakeResult,
    ) -> LobbyOutcome {
        let player_count = other_names.len() + 1;
        let games = (0..player_count)
            .map(|_| new_game_from_battle_config(handshake.seed, &handshake.config))
            .collect();
        let mut player_names = Vec::with_capacity(player_count);
        player_names.push(self.my_name.clone());
        player_names.extend(other_names);

        match BattleState::from_peer_streams(
            games,
            player_names,
            streams,
            handshake.start_at_unix_ms,
        ) {
            Ok(state) => LobbyOutcome::Battle(Box::new(state)),
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new());
                LobbyOutcome::Stay
            }
        }
    }

    /// 迎え入れ済みのゲストを現在のフェーズから取り出す。`HostedGuest`は`TcpStream`を
    /// 持ちcloneできないため、フェーズを移すときはこれで移送する。
    fn take_guests(&mut self) -> Vec<HostedGuest> {
        match &mut self.phase {
            LobbyPhase::Discovering { guests }
            | LobbyPhase::AwaitingInviteResponse { guests, .. }
            | LobbyPhase::AcceptingGuestConnection { guests, .. }
            | LobbyPhase::Notice { guests, .. } => std::mem::take(guests),
            _ => Vec::new(),
        }
    }

    /// 選択中の候補へ招待を送る。候補が1件も無い、またはルームが既に上限人数
    /// (`ROOM_MAX_PLAYERS`)に達していれば何もしない。
    fn invite_selected(&mut self) {
        if self.hosted_guests().len() + 1 >= ROOM_MAX_PLAYERS {
            return;
        }
        let Some(target) = self.discovery.peers().get(self.selection).cloned() else {
            return;
        };
        if self.discovery.send_invite(&target).is_err() {
            // 送れなかった場合は探索を続ける(相手が既にいなくなった等)。
            return;
        }
        let guests = self.take_guests();
        self.phase = LobbyPhase::AwaitingInviteResponse {
            target,
            sent_at: Instant::now(),
            guests,
        };
    }

    fn move_selection(&mut self, forward: bool) {
        let len = self.discovery.peers().len();
        if len == 0 {
            self.selection = 0;
            return;
        }
        self.selection = if forward {
            (self.selection + 1) % len
        } else {
            (self.selection + len - 1) % len
        };
    }

    /// 候補が減ってカーソルが範囲外になった場合に詰める。
    fn clamp_selection(&mut self) {
        let len = self.discovery.peers().len();
        if self.selection >= len {
            self.selection = len.saturating_sub(1);
        }
    }
}

/// ゲストを迎えていない探索フェーズ。
fn discovering() -> LobbyPhase {
    LobbyPhase::Discovering { guests: Vec::new() }
}

/// 短い通知フェーズを作る。`guests`は通知を抜けた後、探索フェーズへそのまま
/// 持ち越す(#284)。
fn notice(message: &str, guests: Vec<HostedGuest>) -> LobbyPhase {
    LobbyPhase::Notice {
        message: message.to_string(),
        shown_at: Instant::now(),
        guests,
    }
}

/// 対戦用のTCP listenerを確保する。既定ポートが使用中なら1つずつ上へ空きを探す
/// (spec.md 12.1)。
fn bind_battle_listener() -> io::Result<TcpListener> {
    let mut last_error = None;
    for port in DEFAULT_TCP_PORT..DEFAULT_TCP_PORT.saturating_add(TCP_PORT_SEARCH_COUNT) {
        match TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))) {
            Ok(listener) => return Ok(listener),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::AddrInUse, "対戦用のTCPポートに空きが無い")
    }))
}

/// 受理したルーム参加接続から`JoinRoom`を1回読み、迎え入れたゲストとして組み立てる。
fn read_join_room(mut room_stream: TcpStream, peer_addr: SocketAddr) -> io::Result<HostedGuest> {
    // listenerが非ブロッキングのため、環境によっては受理したストリームもそれを
    // 引き継ぐ。以降はブロッキング前提なので明示的に戻す。
    room_stream.set_nonblocking(false)?;
    let (name, mesh_port) = match net::read_message(&mut room_stream)? {
        GameMessage::JoinRoom { name, mesh_port } => (name, mesh_port),
        other => return Err(net::unexpected_message("JoinRoom", &other)),
    };
    Ok(HostedGuest {
        // メッシュの宛先は「参加接続の接続元IP」+「`JoinRoom`が申告したポート」
        // (`room::start_room_as_host`のコメント)。
        mesh_addr: SocketAddr::new(peer_addr.ip(), mesh_port),
        room_stream,
        name,
    })
}

#[cfg(test)]
impl LobbyState {
    /// フェーズを直接差し替える。描画のテストで、通信を挟まずに各フェーズの
    /// 見え方を確かめるために使う。
    pub(crate) fn set_phase(&mut self, phase: LobbyPhase) {
        self.phase = phase;
    }

    /// 候補を直接1件積む(同上)。
    pub(crate) fn add_peer(&mut self, peer: DiscoveredPeer) {
        self.discovery.add_peer(peer);
    }

    /// テスト用。UDP・TCPともループバックの空きポートで開始する(固定ポートは
    /// 同一プロセスで2インスタンス分を確保できないため)。
    pub(crate) fn new_on_loopback(my_name: String) -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        listener.set_nonblocking(true)?;
        let tcp_port = listener.local_addr()?.port();
        let mesh_listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let discovery = Discovery::start_on_loopback(my_name.clone(), tcp_port)?;
        Ok(Self::new_with(discovery, listener, mesh_listener, my_name))
    }
}

#[cfg(test)]
impl HostedGuest {
    /// 描画のテスト用。通信内容は使わないため、ループバックで張った接続を持たせる。
    pub(crate) fn for_test(name: &str) -> Self {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("ループバックのlistenerは取れるはず");
        let addr = listener.local_addr().expect("bind済みのはず");
        let room_stream = TcpStream::connect(addr).expect("自分のlistenerへは繋がるはず");
        Self {
            room_stream,
            name: name.to_string(),
            mesh_addr: addr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    /// 対戦成立まで待つフレーム数の上限(1フレーム5ms)。メッシュ確立は参加者の
    /// スレッドを待ち合わせるため、負荷の高い環境でも間に合う長さにしておく。
    const MAX_PUMPS: usize = 1000;

    /// テスト用の対戦設定。盤面生成を軽くするため短いコース(20m)にする。
    fn test_config() -> BattleConfig {
        BattleConfig::from_settings(&Settings::default(), 20)
    }

    /// 互いのHELLOが届くよう向かい合わせたN個のロビー(`names`と同じ順)。
    fn facing_lobbies_of(names: &[&str]) -> Vec<LobbyState> {
        let mut lobbies: Vec<LobbyState> = names
            .iter()
            .map(|name| LobbyState::new_on_loopback(name.to_string()).unwrap())
            .collect();
        let addrs: Vec<SocketAddr> = lobbies
            .iter()
            .map(|lobby| lobby.discovery.local_addr())
            .collect();
        for (index, lobby) in lobbies.iter_mut().enumerate() {
            let targets = addrs
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, &addr)| addr)
                .collect();
            lobby.discovery.set_hello_targets(targets);
        }
        lobbies
    }

    /// 2人ぶん。招待する側(=主催者になる)と招待される側(=ゲストになる)。
    fn facing_lobbies() -> (LobbyState, LobbyState) {
        let mut lobbies = facing_lobbies_of(&["host", "guest"]);
        let guest = lobbies.pop().unwrap();
        let host = lobbies.pop().unwrap();
        (host, guest)
    }

    /// 相手を候補リストに載せるまで、両者のHELLOを流し合う。
    fn discover_each_other(host: &mut LobbyState, guest: &mut LobbyState) {
        for _ in 0..200 {
            // 実運用の1秒間隔を実時間で待つとテストが遅くなるため、毎回送らせる。
            host.discovery.resend_hello_now();
            guest.discovery.resend_hello_now();
            host.update(&[], test_config());
            guest.update(&[], test_config());
            if !host.peers().is_empty() && !guest.peers().is_empty() {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// 全員が互いを候補リストに載せるまでHELLOを流し合う(N人版)。
    fn discover_all(lobbies: &mut [LobbyState]) {
        let expected = lobbies.len() - 1;
        for _ in 0..200 {
            for lobby in lobbies.iter_mut() {
                lobby.discovery.resend_hello_now();
                lobby.update(&[], test_config());
            }
            if lobbies.iter().all(|lobby| lobby.peers().len() >= expected) {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// 名前で候補を選んで招待する。ルームに加わったゲストはBYEで候補から消えるため、
    /// 固定のindexでは狙えない。
    fn invite_by_name(lobby: &mut LobbyState, name: &str) {
        let index = lobby
            .peers()
            .iter()
            .position(|peer| peer.player_name == name)
            .unwrap_or_else(|| panic!("候補に{name}がいるはず"));
        lobby.selection = index;
        lobby.update(&[InputAction::Confirm], test_config());
        assert!(
            matches!(lobby.phase(), LobbyPhase::AwaitingInviteResponse { target, .. } if target.player_name == name),
            "招待した側は{name}への応答待ちへ移るはず"
        );
    }

    /// 主催者が`guest`を招待し、ゲストがルームへ加わるまで両者を回す。
    fn invite_and_join(host: &mut LobbyState, guest: &mut LobbyState, guest_name: &str) {
        let joined_before = host.hosted_guests().len();
        invite_by_name(host, guest_name);

        for _ in 0..MAX_PUMPS {
            // ゲストは招待が届いたら承諾する。
            let actions: &[InputAction] =
                if matches!(guest.phase(), LobbyPhase::IncomingInvite { .. }) {
                    &[InputAction::Confirm]
                } else {
                    &[]
                };
            guest.update(actions, test_config());
            host.update(&[], test_config());

            if host.hosted_guests().len() > joined_before
                && matches!(guest.phase(), LobbyPhase::WaitingForRoomStart { .. })
            {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("{guest_name}がルームへ加わらなかった");
    }

    /// 対戦が成立するまでロビーを回し、成立したら`player_names`を返す。
    /// `first_actions`は最初の1回だけ渡す操作(承諾のEnter・開始のTab等)。
    fn run_until_battle(
        lobby: &mut LobbyState,
        first_actions: &[InputAction],
    ) -> Option<Vec<String>> {
        let mut actions = first_actions.to_vec();
        for _ in 0..MAX_PUMPS {
            match lobby.update(&actions, test_config()) {
                LobbyOutcome::Battle(state) => return Some(state.player_names.clone()),
                LobbyOutcome::Leave => return None,
                LobbyOutcome::Stay => {}
            }
            actions.clear();
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// `names[0]`が主催者・以降がゲストとしてロビーを通しで回し、全員の`player_names`を
    /// `names`と同じ順で返す。探索→招待→承諾→ルーム参加→開始(Tab)→メッシュ確立まで
    /// 実際の通信で進める。
    fn run_room_of(names: &[&str]) -> Vec<Vec<String>> {
        let mut lobbies = facing_lobbies_of(names);
        discover_all(&mut lobbies);
        for (lobby, name) in lobbies.iter().zip(names) {
            assert_eq!(
                lobby.peers().len(),
                names.len() - 1,
                "前提: {name}は他の全員を見つけているはず"
            );
        }

        let mut guests = lobbies.split_off(1);
        let mut host = lobbies.pop().unwrap();

        // 1人ずつ招待し、加わるのを待ってから次を招待する(加わった順がroom内
        // インデックス=`player_names`の並びになる)。
        for (guest, name) in guests.iter_mut().zip(&names[1..]) {
            invite_and_join(&mut host, guest, name);
        }
        assert_eq!(host.hosted_guests().len(), names.len() - 1);
        let joined: Vec<&str> = host
            .hosted_guests()
            .iter()
            .map(|guest| guest.name())
            .collect();
        assert_eq!(joined, names[1..], "迎え入れた順は招待した順のはず");

        // 開始操作(Tab)はメッシュ確立までブロックするため、ゲスト側は別スレッドで回す。
        let guest_threads: Vec<_> = guests
            .into_iter()
            .map(|mut guest| thread::spawn(move || run_until_battle(&mut guest, &[])))
            .collect();
        let host_names = run_until_battle(&mut host, &[InputAction::StartRoom]);

        let mut all = vec![host_names.expect("主催者は対戦を開始するはず")];
        for (index, thread) in guest_threads.into_iter().enumerate() {
            let names_seen = thread.join().expect("ゲストのスレッドは正常終了するはず");
            all.push(
                names_seen.unwrap_or_else(|| panic!("{}は対戦を開始するはず", names[index + 1])),
            );
        }
        all
    }

    #[test]
    fn a_lobby_starts_out_discovering_with_no_candidates() {
        let lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        assert!(matches!(lobby.phase(), LobbyPhase::Discovering { .. }));
        assert!(lobby.peers().is_empty());
        assert!(lobby.hosted_guests().is_empty());
        assert_eq!(lobby.selection(), 0);
    }

    #[test]
    fn confirming_without_any_candidate_keeps_the_lobby_discovering() {
        // 候補が1件も無い状態でEnterを押しても、送る相手がいないので何も起きない。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        let outcome = lobby.update(&[InputAction::Confirm], test_config());

        assert!(matches!(outcome, LobbyOutcome::Stay));
        assert!(matches!(lobby.phase(), LobbyPhase::Discovering { .. }));
    }

    #[test]
    fn starting_a_room_without_any_guest_does_nothing() {
        // #276: 参加者が1人もいなければTabは無効(1人で対戦は始められない)。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        let outcome = lobby.update(&[InputAction::StartRoom], test_config());

        assert!(matches!(outcome, LobbyOutcome::Stay));
        assert!(matches!(lobby.phase(), LobbyPhase::Discovering { .. }));
    }

    #[test]
    fn inviting_a_candidate_does_nothing_once_the_room_is_already_full() {
        // #283: ルームがすでに上限人数(自分+ゲスト3人=4人)に達していれば、候補に
        // Confirmしても新たな招待は送らない(誤操作で5人目を招待できてしまうのを防ぐ)。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.add_peer(DiscoveredPeer::for_test(
            "candidate",
            std::net::IpAddr::from(Ipv4Addr::new(192, 168, 0, 9)),
            39399,
        ));
        lobby.set_phase(LobbyPhase::Discovering {
            guests: vec![
                HostedGuest::for_test("g1"),
                HostedGuest::for_test("g2"),
                HostedGuest::for_test("g3"),
            ],
        });

        lobby.update(&[InputAction::Confirm], test_config());

        assert!(
            matches!(lobby.phase(), LobbyPhase::Discovering { guests } if guests.len() == 3),
            "上限に達している間はConfirmしても応答待ちへ移らないはず"
        );
    }

    #[test]
    fn escape_while_discovering_leaves_the_lobby() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        let outcome = lobby.update(&[InputAction::Quit], test_config());

        assert!(matches!(outcome, LobbyOutcome::Leave));
    }

    #[test]
    fn the_selection_cycles_through_the_candidates_in_both_directions() {
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);
        assert_eq!(host.peers().len(), 1, "前提: 相手を1件見つけているはず");

        // 候補が1件しか無いので、どちらへ動かしても位置は変わらない。
        host.update(&[InputAction::FaceDown], test_config());
        assert_eq!(host.selection(), 0);
        host.update(&[InputAction::FaceUp], test_config());
        assert_eq!(host.selection(), 0);
    }

    #[test]
    fn inviting_a_candidate_moves_to_awaiting_and_shows_up_as_an_incoming_invite() {
        // 招待した側は応答待ち、された側は承諾を聞く画面へ移る。
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);

        host.update(&[InputAction::Confirm], test_config());
        assert!(
            matches!(host.phase(), LobbyPhase::AwaitingInviteResponse { target, .. } if target.player_name == "guest"),
            "招待した側は応答待ちへ移るはず"
        );

        for _ in 0..200 {
            guest.update(&[], test_config());
            if matches!(guest.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(guest.phase(), LobbyPhase::IncomingInvite { from } if from.player_name == "host"),
            "招待された側は承諾を聞く画面へ移るはず"
        );
    }

    #[test]
    fn declining_an_invite_sends_both_sides_back_toward_discovering() {
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);

        host.update(&[InputAction::Confirm], test_config());
        for _ in 0..200 {
            guest.update(&[], test_config());
            if matches!(guest.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(guest.phase(), LobbyPhase::IncomingInvite { .. }));

        // Escで拒否する。拒否した側はすぐ探索へ戻り、招待した側は通知を経て戻る。
        guest.update(&[InputAction::Quit], test_config());
        assert!(matches!(guest.phase(), LobbyPhase::Discovering { .. }));

        for _ in 0..200 {
            host.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::Notice { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(host.phase(), LobbyPhase::Notice { message, .. } if message == "相手に断られました"),
            "断られたことが通知として出るはず"
        );
    }

    #[test]
    fn an_invite_that_is_never_answered_times_out_into_a_notice() {
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);
        host.update(&[InputAction::Confirm], test_config());

        // 実時間で10秒待つ代わりに、送信時刻をタイムアウトぶん過去へ倒す。
        let target = match &host.phase {
            LobbyPhase::AwaitingInviteResponse { target, .. } => target.clone(),
            _ => panic!("前提: 応答待ちのはず"),
        };
        host.phase = LobbyPhase::AwaitingInviteResponse {
            target,
            sent_at: Instant::now() - Duration::from_millis(INVITE_TIMEOUT_MS),
            guests: Vec::new(),
        };
        host.update(&[], test_config());

        assert!(
            matches!(host.phase(), LobbyPhase::Notice { message, .. } if message == "応答がありませんでした")
        );
    }

    #[test]
    fn an_invite_that_arrives_while_hosting_a_room_is_declined_automatically() {
        // 設計書に無い判断(lobby.rsのコメント参照): ルームを開いている最中に招待されても、
        // 迎え入れ済みのゲストを黙って切断しないよう自動で断る。
        let (mut host, mut other) = facing_lobbies();
        discover_each_other(&mut host, &mut other);
        host.set_phase(LobbyPhase::Discovering {
            guests: vec![HostedGuest::for_test("joined")],
        });

        invite_by_name(&mut other, "host");
        for _ in 0..200 {
            host.update(&[], test_config());
            other.update(&[], test_config());
            if matches!(other.phase(), LobbyPhase::Notice { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        assert!(
            matches!(host.phase(), LobbyPhase::Discovering { guests } if guests.len() == 1),
            "主催者は開いたルームを保ったままのはず"
        );
        assert!(
            matches!(other.phase(), LobbyPhase::Notice { message, .. } if message == "相手に断られました"),
            "招待した側には断られた通知が出るはず"
        );
    }

    #[test]
    fn a_notice_returns_to_discovering_once_it_has_been_shown_long_enough() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.phase = LobbyPhase::Notice {
            message: "テスト".to_string(),
            shown_at: Instant::now() - Duration::from_millis(NOTICE_DISPLAY_MS),
            guests: Vec::new(),
        };

        lobby.update(&[], test_config());

        assert!(matches!(lobby.phase(), LobbyPhase::Discovering { .. }));
    }

    #[test]
    fn a_notice_carries_the_already_hosted_guests_back_to_discovering() {
        // #284: 招待の失敗・タイムアウト・接続失敗1つで、既に迎え入れていた
        // ゲストまでルームから失われないようにする。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.phase = LobbyPhase::Notice {
            message: "テスト".to_string(),
            shown_at: Instant::now() - Duration::from_millis(NOTICE_DISPLAY_MS),
            guests: vec![HostedGuest::for_test("already-joined")],
        };

        lobby.update(&[], test_config());

        assert!(
            matches!(lobby.phase(), LobbyPhase::Discovering { guests } if guests.len() == 1 && guests[0].name() == "already-joined"),
            "通知を抜けた後も既に迎えていたゲストが残っているはず"
        );
    }

    #[test]
    fn accepting_an_invite_connects_both_sides_and_starts_a_battle() {
        // 探索→招待→承諾→ルーム参加→開始(Tab)→対戦成立までを通しで確認する。
        // #276で役割が反転し、招待した側が主催者(サーバ役)・承諾した側がゲスト
        // (クライアント役)になる。
        let names = run_room_of(&["host", "guest"]);

        assert_eq!(
            names[0],
            vec!["host".to_string(), "guest".to_string()],
            "主催者から見た並びはindex 0が自分・1がゲスト"
        );
        assert_eq!(
            names[1],
            vec!["guest".to_string(), "host".to_string()],
            "ゲストから見た並びもindex 0が自分・1が主催者"
        );
    }

    #[test]
    fn a_room_of_three_players_starts_a_battle_for_everyone() {
        // #276: 主催者が2人を順に招待して開始する。全員のindex 0が自分で、残りは
        // room内インデックス順(主催者→ゲスト1→ゲスト2から自分を除いたもの)。
        let names = run_room_of(&["host", "guest-1", "guest-2"]);

        assert_eq!(names[0], vec!["host", "guest-1", "guest-2"]);
        assert_eq!(names[1], vec!["guest-1", "host", "guest-2"]);
        assert_eq!(names[2], vec!["guest-2", "host", "guest-1"]);
    }

    #[test]
    fn a_room_of_four_players_starts_a_battle_for_everyone() {
        // #276: 4人(#273-275のフルメッシュ上限)まで同じ手順で集められる。
        let names = run_room_of(&["host", "guest-1", "guest-2", "guest-3"]);

        assert_eq!(names[0], vec!["host", "guest-1", "guest-2", "guest-3"]);
        assert_eq!(names[1], vec!["guest-1", "host", "guest-2", "guest-3"]);
        assert_eq!(names[2], vec!["guest-2", "host", "guest-1", "guest-3"]);
        assert_eq!(names[3], vec!["guest-3", "host", "guest-1", "guest-2"]);
    }
}
