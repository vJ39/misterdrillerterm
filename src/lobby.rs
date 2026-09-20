//! 対戦相手を探して参加リクエストをやり取りするロビー画面の状態(#256。spec.md 12.1)。
//!
//! UDP探索(`discovery.rs`)の候補リストと、参加リクエスト→ルーム参加→フルメッシュ確立
//! (`room.rs`)→対戦開始(`battle.rs`)までの流れを1つの状態機械にまとめる。入力の取り込みと
//! 描画は画面側(`app::screens::tick_network_lobby`・`ui::render::draw_network_lobby`)が行い、
//! ここは「押された操作」と「経過時間」を受け取ってフェーズを進めることに専念する
//! (ターミナルを持たずに結合テストできるようにするため)。
//!
//! 役割は「参加リクエストを送った側が常にゲスト(TCPクライアント役)・受けた側が
//! ホスト(TCPサーバ役)」で固定する(#293。docs/multiplayer-lobby-join-request-redesign.md。
//! #276で導入した「招待した側が主催者」から反転させたもの)。ホストは探索リストに
//! いる間ずっと募集中で、届いたリクエストを1件ずつ確認してゲストを迎え入れる。
//! 同時に複数届いた場合は`pending`へ積み、順番に確認する。対戦の開始操作(Tab)は
//! ホストとゲストのどちらからでも出せる。

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rand::RngExt;

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

/// AI対戦で選べるAIの人数の範囲(#296)。自分を足した合計が`ROOM_MAX_PLAYERS`を
/// 超えないようにするため、上限は「最大人数-1」(=3人。合計2〜4人)。
const AI_OPPONENT_COUNT_RANGE: std::ops::RangeInclusive<usize> = 1..=(ROOM_MAX_PLAYERS - 1);

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
    /// 候補を探しながら参加リクエストを待っている通常状態。`guests`が空でなければ、
    /// 既にルームを開いていて、さらに参加リクエストを受けられる状態(#293)。
    ///
    /// `ai_count`はこのルームへ混ぜるAIの人数(#300。初期0)。ホストがここで増減し、
    /// 開始操作(Tab)のときに`room::start_room_as_host`へ渡す。#296のAI対戦(V)と違い
    /// 通信ありのルームの話で、人間の参加者と混在させられる。
    Discovering {
        guests: Vec<HostedGuest>,
        ai_count: usize,
    },
    /// 自分から参加リクエストを送り、相手の応答を待っている(#293で名前は維持)。
    AwaitingInviteResponse {
        target: DiscoveredPeer,
        sent_at: Instant,
        guests: Vec<HostedGuest>,
    },
    /// 参加リクエストを受け取り、許可するか聞いている。
    ///
    /// `pending`は他に届いている未処理の参加リクエスト(#293。1件ずつ順番に処理し、
    /// #291のように無応答で待たされる相手を出さない)。
    IncomingInvite {
        from: DiscoveredPeer,
        pending: Vec<DiscoveredPeer>,
        guests: Vec<HostedGuest>,
    },
    /// ACCEPTを返したゲストのTCP接続を待っている(ホスト側)。
    ///
    /// `pending`はここにも持たせ、接続待ち中に届いた参加リクエストを取り逃さない(#293)。
    AcceptingGuestConnection {
        guest_name: String,
        started: Instant,
        pending: Vec<DiscoveredPeer>,
        guests: Vec<HostedGuest>,
    },
    /// 参加リクエストが許可され、ホストへの接続を試みている(ゲスト側)。
    ///
    /// `host_peer`は開始要求(REQUEST_START)の送信先として持ち越す(#293)。
    ConnectingToHost {
        addr: SocketAddr,
        host_name: String,
        host_peer: DiscoveredPeer,
    },
    /// ホストへ接続・`JoinRoom`送信済みで、開始(`RoomRoster`以降)を別スレッドで待っている。
    /// このフェーズでStartRoom操作を受け付け、ホストへ開始要求を送る(#293)。
    WaitingForRoomStart {
        result_rx: mpsc::Receiver<io::Result<room::RoomStartResult>>,
        host_peer: DiscoveredPeer,
    },
    /// AIと対戦する人数を選んでいる(#296)。`ai_count`は1〜3(合計2〜4人)。
    /// 通信は一切使わないため、既に迎え入れたゲスト(`guests`)があっても無視する
    /// (対戦の種類が違うため両立しない)。
    SelectingAiOpponentCount { ai_count: usize },
    /// 拒否・タイムアウト・接続失敗を短く伝える。時間が経つと探索へ戻る。
    ///
    /// `guests`はホスト側で既に迎え入れていたゲスト(#284。リクエストや接続の失敗1つで
    /// ルーム全体を解散させないよう、通知を挟んでも持ち越す)。ゲスト側の失敗
    /// (このロビー自身がまだ誰も迎えていない)では空になる。`pending`も同じ理由で
    /// 持ち越す(#293)。
    Notice {
        message: String,
        shown_at: Instant,
        guests: Vec<HostedGuest>,
        pending: Vec<DiscoveredPeer>,
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
    /// 参加リクエストが届いた。許可するか聞く。
    IncomingInvite(DiscoveredPeer),
    /// ルームが満員の最中に参加リクエストが届いた。返信だけして状態は変えない。
    DeclineWhileHosting(DiscoveredPeer),
    /// 別のリクエストの処理中に参加リクエストが届いた。順番待ちへ積む(#293)。
    QueueRequest(DiscoveredPeer),
    /// 自分の参加リクエストが許可された。ホストへの接続へ移る(#293で自分はゲスト)。
    /// 接続先と開始要求の送信先の両方に使うため、相手の候補情報ごと載せる。
    InviteAccepted(DiscoveredPeer),
    /// 参加リクエストが断られた。
    InviteDeclined,
    /// ゲストから開始要求が届いた。追認なしに即座に開始する(#293)。
    StartRequested,
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
            LobbyPhase::Discovering { guests, .. }
            | LobbyPhase::AwaitingInviteResponse { guests, .. }
            | LobbyPhase::IncomingInvite { guests, .. }
            | LobbyPhase::AcceptingGuestConnection { guests, .. } => guests,
            _ => &[],
        }
    }

    /// このルームへ混ぜるAI(#300)の人数。持たないフェーズでは0(AIを追加しない)。
    pub fn room_ai_count(&self) -> usize {
        match &self.phase {
            LobbyPhase::Discovering { ai_count, .. } => *ai_count,
            _ => 0,
        }
    }

    /// 1フレーム分進める。`actions`はこのフレームに届いた操作、`config`は自分が
    /// ホストになった場合に参加者へ強制適用する設定(spec.md 12.2)。
    pub fn update(&mut self, actions: &[InputAction], config: BattleConfig) -> LobbyOutcome {
        let packets = self.discovery.tick();
        // ゲストからの開始要求で対戦が成立することがあるため、結果を持ち帰る(#293)。
        if let Some(outcome) = self.apply_packets(packets, config) {
            return outcome;
        }
        self.clamp_selection();

        for &action in actions {
            if let Some(outcome) = self.apply_action(action, config) {
                return outcome;
            }
        }

        self.advance_phase()
    }

    /// 受信した自分宛のINVITE/ACCEPT/DECLINE/REQUEST_STARTを、現在のフェーズに応じて
    /// 反映する。開始要求で対戦が成立した場合だけ`Some`を返す(#293)。
    fn apply_packets(
        &mut self,
        packets: Vec<DiscoveryPacket>,
        config: BattleConfig,
    ) -> Option<LobbyOutcome> {
        for packet in packets {
            match self.packet_effect(&packet) {
                Some(PacketEffect::IncomingInvite(from)) => {
                    let guests = self.take_guests();
                    let pending = self.take_pending();
                    self.phase = LobbyPhase::IncomingInvite {
                        from,
                        pending,
                        guests,
                    };
                }
                Some(PacketEffect::DeclineWhileHosting(from)) => {
                    // ルームが満員の最中の参加リクエストは自動で断る。無視すると相手は
                    // タイムアウト(`INVITE_TIMEOUT_MS`)まで無応答で待たされるため。
                    let _ = self.discovery.send_decline(&from);
                }
                Some(PacketEffect::QueueRequest(from)) => self.push_pending(from),
                Some(PacketEffect::InviteAccepted(host)) => {
                    // 参加リクエストを送った側はゲスト(TCPクライアント役)になる(#293)。
                    // ルームに入るので自分の募集はここで止める。
                    self.discovery.send_bye();
                    self.phase = LobbyPhase::ConnectingToHost {
                        // 接続先はホストのIPと、HELLOで広告されていたTCPポート。
                        addr: SocketAddr::new(host.addr, host.tcp_port),
                        host_name: host.player_name.clone(),
                        host_peer: host,
                    };
                }
                Some(PacketEffect::InviteDeclined) => {
                    let guests = self.take_guests();
                    self.phase = notice("相手に断られました", guests, Vec::new());
                }
                // ホストの追認なしに即座に開始する(#293)。
                Some(PacketEffect::StartRequested) => return Some(self.start_room(config)),
                None => {}
            }
        }
        None
    }

    /// パケット1つを現在のフェーズと突き合わせ、何をするか決める(状態は変えない)。
    fn packet_effect(&self, packet: &DiscoveryPacket) -> Option<PacketEffect> {
        match (&self.phase, packet.packet_type) {
            // 参加リクエストが届いた。ゲストが既にいても受け付ける(N人対戦なので、
            // 満員(`ROOM_MAX_PLAYERS`)になるまでは追加で迎え入れられる。#293)。
            (LobbyPhase::Discovering { guests, .. }, PacketType::Invite) => {
                let from = self.peer_of(packet)?;
                if guests.len() + 1 >= ROOM_MAX_PLAYERS {
                    Some(PacketEffect::DeclineWhileHosting(from))
                } else {
                    Some(PacketEffect::IncomingInvite(from))
                }
            }
            // 別の参加リクエストの検討中(まだ許可/拒否の返事をしていない)に来たもの。
            // 順番待ちへ積んで、今のリクエストを処理した後に確認する(#293)。
            (
                LobbyPhase::IncomingInvite {
                    from: current,
                    pending,
                    ..
                },
                PacketType::Invite,
            ) => {
                // 同じ相手からの再送(HELLO間隔で何度も押された等)は積まない。
                if current.sender_id == packet.sender_id || is_queued(pending, packet) {
                    return None;
                }
                Some(PacketEffect::QueueRequest(self.peer_of(packet)?))
            }
            // ゲストの接続待ち中・通知表示中に来たものも取りこぼさず積む(#293)。
            (LobbyPhase::AcceptingGuestConnection { pending, .. }, PacketType::Invite)
            | (LobbyPhase::Notice { pending, .. }, PacketType::Invite) => {
                if is_queued(pending, packet) {
                    return None;
                }
                Some(PacketEffect::QueueRequest(self.peer_of(packet)?))
            }
            (LobbyPhase::AwaitingInviteResponse { target, .. }, PacketType::Accept)
                if target.sender_id == packet.sender_id =>
            {
                Some(PacketEffect::InviteAccepted(target.clone()))
            }
            (LobbyPhase::AwaitingInviteResponse { target, .. }, PacketType::Decline)
                if target.sender_id == packet.sender_id =>
            {
                Some(PacketEffect::InviteDeclined)
            }
            // ゲストからの開始要求。迎え入れたゲストが1人もいなければ意味が無いので無視する。
            (LobbyPhase::Discovering { guests, .. }, PacketType::RequestStart)
            | (LobbyPhase::AcceptingGuestConnection { guests, .. }, PacketType::RequestStart) => {
                if guests.is_empty() {
                    None
                } else {
                    Some(PacketEffect::StartRequested)
                }
            }
            _ => None,
        }
    }

    /// パケットの送り主を候補リストから引く。HELLOを1秒間隔で流し合っているため
    /// 載っているはずで、載っていなければ返信先が分からないので扱わない。
    fn peer_of(&self, packet: &DiscoveryPacket) -> Option<DiscoveredPeer> {
        self.discovery
            .peers()
            .iter()
            .find(|peer| peer.sender_id == packet.sender_id)
            .cloned()
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
                // AI対戦は通信を使わないため、既にゲストを迎え入れている場合は
                // 無視する(#296。誤操作でゲストの接続を切ってしまわないように)。
                InputAction::StartAiBattle => {
                    if !has_guests {
                        self.phase = LobbyPhase::SelectingAiOpponentCount { ai_count: 1 };
                    }
                }
                // このルームへ混ぜるAIの枠の増減(#300)。#296のVと違い通信ありの
                // ルームの話なので、ゲストがいてもいなくても操作できる。
                InputAction::IncreaseRoomAiCount => self.adjust_room_ai_count(true),
                InputAction::DecreaseRoomAiCount => self.adjust_room_ai_count(false),
                InputAction::Quit => {
                    // 相手の候補リストから即座に消えるよう、抜ける前にBYEを流す。
                    self.discovery.send_bye();
                    return Some(LobbyOutcome::Leave);
                }
                _ => {}
            },
            // 参加リクエストのキャンセル。探索自体は続けるためBYEは送らない。
            // 迎え入れ済みのゲストは持ち越す。
            LobbyPhase::AwaitingInviteResponse { .. } => {
                if action == InputAction::Quit {
                    let guests = self.take_guests();
                    self.phase = discovering_with(guests);
                }
            }
            LobbyPhase::IncomingInvite { from, .. } => match action {
                // Enter=許可、Esc=拒否(既存の入力体系に合わせる)。
                InputAction::Confirm => {
                    let guest = from.clone();
                    if self.discovery.send_accept(&guest).is_err() {
                        let guests = self.take_guests();
                        let pending = self.take_pending();
                        self.phase = notice(CONNECT_FAILED_MESSAGE, guests, pending);
                        return None;
                    }
                    // 参加リクエストを受けた側はホスト(TCPサーバ役)のまま、相手の接続を
                    // 待つ(#293)。募集は続けるのでBYEは送らない(止めるのは開始する時)。
                    let guests = self.take_guests();
                    let pending = self.take_pending();
                    self.phase = LobbyPhase::AcceptingGuestConnection {
                        guest_name: guest.player_name,
                        started: Instant::now(),
                        pending,
                        guests,
                    };
                }
                InputAction::Quit => {
                    let requester = from.clone();
                    let _ = self.discovery.send_decline(&requester);
                    let guests = self.take_guests();
                    let pending = self.take_pending();
                    self.back_to_discovering(guests, pending);
                }
                _ => {}
            },
            // AI対戦の人数選択(#296)。通信を使わないため、ここでの操作は探索・招待の
            // やり取りへ一切影響しない。
            LobbyPhase::SelectingAiOpponentCount { ai_count } => {
                let ai_count = *ai_count;
                match action {
                    InputAction::FaceUp => {
                        self.phase = LobbyPhase::SelectingAiOpponentCount {
                            ai_count: (ai_count + 1).min(*AI_OPPONENT_COUNT_RANGE.end()),
                        };
                    }
                    InputAction::FaceDown => {
                        self.phase = LobbyPhase::SelectingAiOpponentCount {
                            ai_count: ai_count
                                .saturating_sub(1)
                                .max(*AI_OPPONENT_COUNT_RANGE.start()),
                        };
                    }
                    InputAction::Confirm => {
                        return Some(self.start_ai_battle(ai_count, config));
                    }
                    InputAction::Quit => {
                        let guests = self.take_guests();
                        self.phase = discovering_with(guests);
                    }
                    _ => {}
                }
            }
            // ゲストからも開始できる。ホストへ開始要求を送るだけで、状態は変えない
            // (開始はホストが配る`RoomRoster`以降で進む。#293)。
            LobbyPhase::WaitingForRoomStart { host_peer, .. } => {
                if action == InputAction::StartRoom {
                    let _ = self.discovery.send_request_start(host_peer);
                }
            }
            // 接続中・通知表示中は操作を受け付けない。
            LobbyPhase::AcceptingGuestConnection { .. }
            | LobbyPhase::ConnectingToHost { .. }
            | LobbyPhase::Notice { .. } => {}
        }

        None
    }

    /// 時間経過・接続の進行によるフェーズの更新。ルーム開始(=`config`を使う経路)は
    /// Tabキーの操作から呼ぶため、ここでは設定を受け取らない。
    fn advance_phase(&mut self) -> LobbyOutcome {
        match &self.phase {
            // 参加リクエストを受けた側には期限を設けない(送った側が`INVITE_TIMEOUT_MS`で
            // 諦めるため、両側に時計を持たせる必要は無い)。
            // AI対戦の人数選択(#296)にも期限は設けない(通信相手を待たないため)。
            LobbyPhase::Discovering { .. }
            | LobbyPhase::IncomingInvite { .. }
            | LobbyPhase::SelectingAiOpponentCount { .. } => LobbyOutcome::Stay,
            LobbyPhase::AwaitingInviteResponse { sent_at, .. } => {
                let sent_at = *sent_at;
                if sent_at.elapsed() >= Duration::from_millis(INVITE_TIMEOUT_MS) {
                    let guests = self.take_guests();
                    self.phase = notice("応答がありませんでした", guests, Vec::new());
                }
                LobbyOutcome::Stay
            }
            LobbyPhase::AcceptingGuestConnection { started, .. } => {
                let started = *started;
                self.accept_guest(started);
                LobbyOutcome::Stay
            }
            LobbyPhase::ConnectingToHost {
                addr, host_peer, ..
            } => {
                let addr = *addr;
                let host_peer = host_peer.clone();
                self.connect_to_host(addr, host_peer)
            }
            LobbyPhase::WaitingForRoomStart { .. } => self.receive_room_start(),
            LobbyPhase::Notice { shown_at, .. } => {
                let shown_at = *shown_at;
                if shown_at.elapsed() >= Duration::from_millis(NOTICE_DISPLAY_MS) {
                    let guests = self.take_guests();
                    let pending = self.take_pending();
                    self.back_to_discovering(guests, pending);
                }
                LobbyOutcome::Stay
            }
        }
    }

    /// ホストとして、ACCEPTを返したゲストのルーム参加接続を受け入れる(非ブロッキング
    /// のため毎フレーム1回試す)。`JoinRoom`の読み取りだけは短時間のブロッキングで
    /// 済ませる(設計書4節)。
    fn accept_guest(&mut self, started: Instant) {
        match self.listener.accept() {
            Ok((stream, peer_addr)) => match read_join_room(stream, peer_addr) {
                Ok(guest) => {
                    let mut guests = self.take_guests();
                    guests.push(guest);
                    let pending = self.take_pending();
                    self.back_to_discovering(guests, pending);
                }
                Err(_) => {
                    // このゲストの接続には失敗したが、既に迎え入れていた他のゲストは
                    // 持ち越す(#284)。
                    let guests = self.take_guests();
                    let pending = self.take_pending();
                    self.phase = notice(CONNECT_FAILED_MESSAGE, guests, pending);
                }
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= Duration::from_millis(TCP_CONNECT_TIMEOUT_MS) {
                    let guests = self.take_guests();
                    let pending = self.take_pending();
                    self.phase = notice(CONNECT_FAILED_MESSAGE, guests, pending);
                }
            }
            Err(_) => {
                let guests = self.take_guests();
                let pending = self.take_pending();
                self.phase = notice(CONNECT_FAILED_MESSAGE, guests, pending);
            }
        }
    }

    /// 集まったゲストで対戦を開始する(ホスト)。`room::start_room_as_host`はメッシュ
    /// 確立まで進むためブロッキングだが、対戦成立までの一度きりの処理として扱う
    /// (設計書4節)。
    fn start_room(&mut self, config: BattleConfig) -> LobbyOutcome {
        // 開始したら募集は終わり(spec.md 12.1)。
        self.discovery.send_bye();

        // AIの枠(#300)はフェーズが変わる前に読む(`take_guests`で`Discovering`を抜ける)。
        let ai_count = self.room_ai_count();
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
            ai_count,
            &self.my_name,
            &self.mesh_listener,
            config,
        );
        match started {
            Ok((streams, handshake)) => {
                // AIはrosterのゲストの後ろに並ぶ(#300)。参加者名もその並びに合わせる。
                let mut other_names = guest_names;
                other_names.extend((1..=ai_count).map(room::ai_member_name));
                // ホストのroom内インデックスは常に0(設計書4節)。
                self.battle_from_room(streams, other_names, 0, handshake)
            }
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new(), Vec::new());
                LobbyOutcome::Stay
            }
        }
    }

    /// AI対戦を開始する(#296)。通信は一切使わない。人間とAI全員を同じシード・
    /// 同じ設定で作る(spec.md 12.2と同じ考え方=盤面の地形とアイテム配置を揃える
    /// ため)。
    fn start_ai_battle(&mut self, ai_count: usize, config: BattleConfig) -> LobbyOutcome {
        let seed: u64 = rand::rng().random();
        let human_game = new_game_from_battle_config(seed, &config);
        let ai_games = (0..ai_count)
            .map(|_| new_game_from_battle_config(seed, &config))
            .collect();
        let state = BattleState::new_local_vs_ai(human_game, ai_games, self.my_name.clone());
        LobbyOutcome::Battle(Box::new(state))
    }

    /// ゲストとしてホストへ接続し、`JoinRoom`を送る。接続は`connect_timeout`自体が
    /// 待ち時間を持つため1回で決着させ、その後の「ホストが開始するのを待つ」区間は
    /// いつ終わるか分からないため別スレッドへ載せる(設計書5節)。
    fn connect_to_host(&mut self, addr: SocketAddr, host_peer: DiscoveredPeer) -> LobbyOutcome {
        // メッシュ用listenerは待ち受けスレッドへ渡すため複製する(自分は以降使わないが、
        // ロビーの持ち物として開いたままにしておく)。
        let joined = self.mesh_listener.try_clone().and_then(|mesh_listener| {
            let room_stream = room::connect_and_join_room(addr, &self.my_name, &mesh_listener)?;
            Ok((room_stream, mesh_listener))
        });
        let (room_stream, mesh_listener) = match joined {
            Ok(joined) => joined,
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new(), Vec::new());
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
        self.phase = LobbyPhase::WaitingForRoomStart {
            result_rx,
            host_peer,
        };
        LobbyOutcome::Stay
    }

    /// ホストの開始を待っているスレッドから結果を受け取る(まだ届いていなければ待つ)。
    fn receive_room_start(&mut self) -> LobbyOutcome {
        let received = match &self.phase {
            LobbyPhase::WaitingForRoomStart { result_rx, .. } => result_rx.try_recv(),
            _ => return LobbyOutcome::Stay,
        };

        match received {
            Ok(Ok((streams, other_names, my_index, handshake))) => {
                self.battle_from_room(streams, other_names, my_index, handshake)
            }
            // 待ち受けスレッドが失敗した場合と、結果を送らずに終わった場合。
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new(), Vec::new());
                LobbyOutcome::Stay
            }
            Err(mpsc::TryRecvError::Empty) => LobbyOutcome::Stay,
        }
    }

    /// 確立したメッシュ接続と参加者名(自分以外、room内インデックス順)から対戦状態を
    /// 組み立てる。盤面は全員が同じシード・同じ設定で作る(spec.md 12.2)。
    ///
    /// `streams`の`None`はAIの枠(#300)で、`my_index`は自分のroom内インデックス
    /// (ホストは常に0)。どちらも`BattleState`が代理送信の宛先変換に使う。
    fn battle_from_room(
        &mut self,
        streams: Vec<Option<TcpStream>>,
        other_names: Vec<String>,
        my_index: usize,
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
            my_index,
            handshake.start_at_unix_ms,
        ) {
            Ok(state) => LobbyOutcome::Battle(Box::new(state)),
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE, Vec::new(), Vec::new());
                LobbyOutcome::Stay
            }
        }
    }

    /// 迎え入れ済みのゲストを現在のフェーズから取り出す。`HostedGuest`は`TcpStream`を
    /// 持ちcloneできないため、フェーズを移すときはこれで移送する。
    fn take_guests(&mut self) -> Vec<HostedGuest> {
        match &mut self.phase {
            LobbyPhase::Discovering { guests, .. }
            | LobbyPhase::AwaitingInviteResponse { guests, .. }
            | LobbyPhase::IncomingInvite { guests, .. }
            | LobbyPhase::AcceptingGuestConnection { guests, .. }
            | LobbyPhase::Notice { guests, .. } => std::mem::take(guests),
            _ => Vec::new(),
        }
    }

    /// 順番待ちの参加リクエストを現在のフェーズから取り出す(`take_guests`と同じ理由で
    /// フェーズ間の移送に使う。#293)。
    fn take_pending(&mut self) -> Vec<DiscoveredPeer> {
        match &mut self.phase {
            LobbyPhase::IncomingInvite { pending, .. }
            | LobbyPhase::AcceptingGuestConnection { pending, .. }
            | LobbyPhase::Notice { pending, .. } => std::mem::take(pending),
            _ => Vec::new(),
        }
    }

    /// 参加リクエストを順番待ちの末尾へ積む(#293)。持たないフェーズでは何もしない。
    fn push_pending(&mut self, from: DiscoveredPeer) {
        match &mut self.phase {
            LobbyPhase::IncomingInvite { pending, .. }
            | LobbyPhase::AcceptingGuestConnection { pending, .. }
            | LobbyPhase::Notice { pending, .. } => pending.push(from),
            _ => {}
        }
    }

    /// ゲストと順番待ちを持って探索へ戻る。順番待ちが残っていれば、その先頭を次の
    /// 確認画面として出す(#293)。
    ///
    /// 満員(`ROOM_MAX_PLAYERS`)に達していれば、順番待ち全員へ断りを送って捨てる。
    /// 1件ずつ許可していく間にguestsが増えるため、`packet_effect`の人数チェック
    /// (受信時点の人数)だけでは、許可を重ねるうちに上限を超えて迎え入れてしまう。
    fn back_to_discovering(&mut self, guests: Vec<HostedGuest>, pending: Vec<DiscoveredPeer>) {
        if guests.len() + 1 >= ROOM_MAX_PLAYERS {
            for peer in &pending {
                let _ = self.discovery.send_decline(peer);
            }
            self.phase = discovering_with(guests);
            return;
        }
        let mut pending = pending;
        self.phase = if pending.is_empty() {
            discovering_with(guests)
        } else {
            let from = pending.remove(0);
            LobbyPhase::IncomingInvite {
                from,
                pending,
                guests,
            }
        };
    }

    /// 選択中の候補へ参加リクエストを送る(送った側は許可され次第ゲストになる。#293)。
    /// 候補が1件も無い、またはルームが既に上限人数(`ROOM_MAX_PLAYERS`)に達していれば
    /// 何もしない。
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

    /// このルームへ混ぜるAI(#300)の人数を1人増やす/減らす。
    ///
    /// 上限は「自分+ゲスト+AIが`ROOM_MAX_PLAYERS`に収まる人数」。`Discovering`以外の
    /// フェーズでは何もしない(AIの枠を持たないため)。
    fn adjust_room_ai_count(&mut self, increase: bool) {
        let LobbyPhase::Discovering { guests, ai_count } = &mut self.phase else {
            return;
        };
        let max = ROOM_MAX_PLAYERS.saturating_sub(1 + guests.len());
        let next = if increase {
            *ai_count + 1
        } else {
            ai_count.saturating_sub(1)
        };
        *ai_count = next.min(max);
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
    discovering_with(Vec::new())
}

/// 迎え入れ済みのゲストを持って探索フェーズへ戻る。
///
/// AIの枠(#300)は`LobbyPhase::Discovering`だけが持つ値のため、他のフェーズを経由して
/// 戻ってきたときは0(AIを追加しない)に戻る。
fn discovering_with(guests: Vec<HostedGuest>) -> LobbyPhase {
    LobbyPhase::Discovering {
        guests,
        ai_count: 0,
    }
}

/// 短い通知フェーズを作る。`guests`は通知を抜けた後、探索フェーズへそのまま
/// 持ち越す(#284)。`pending`も同様に持ち越し、抜けた後で順番に確認する(#293)。
fn notice(message: &str, guests: Vec<HostedGuest>, pending: Vec<DiscoveredPeer>) -> LobbyPhase {
    LobbyPhase::Notice {
        message: message.to_string(),
        shown_at: Instant::now(),
        guests,
        pending,
    }
}

/// そのパケットの送り主が既に順番待ちに入っているか(同じ相手を二重に積まないため)。
fn is_queued(pending: &[DiscoveredPeer], packet: &DiscoveryPacket) -> bool {
    pending
        .iter()
        .any(|peer| peer.sender_id == packet.sender_id)
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

    /// 2人ぶん。参加リクエストを受ける側(=ホストになる)と送る側(=ゲストになる)。
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

    /// 名前で候補を選んで参加リクエストを送る。ルームに加わったゲストはBYEで候補から
    /// 消えるため、固定のindexでは狙えない。`update`の中で候補リストが入れ替わって
    /// indexがずれることもあるため、狙った相手へ送れるまで選び直す。
    fn invite_by_name(lobby: &mut LobbyState, name: &str) {
        for _ in 0..200 {
            // 先に受信を捌いて候補リストを最新にしてから選ぶ。
            lobby.update(&[], test_config());
            let Some(index) = lobby
                .peers()
                .iter()
                .position(|peer| peer.player_name == name)
            else {
                thread::sleep(Duration::from_millis(2));
                continue;
            };
            lobby.selection = index;
            lobby.update(&[InputAction::Confirm], test_config());
            match lobby.phase() {
                LobbyPhase::AwaitingInviteResponse { target, .. } if target.player_name == name => {
                    return;
                }
                // 直前のtickで候補が入れ替わり別の相手へ送ってしまった場合は取り消す。
                LobbyPhase::AwaitingInviteResponse { .. } => {
                    lobby.update(&[InputAction::Quit], test_config());
                }
                _ => {}
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("{name}へ参加リクエストを送れなかった");
    }

    /// `guest`がホストへ参加リクエストを送り、許可されてルームへ加わるまで両者を回す
    /// (#293で申し込んだ側がゲストになった)。
    fn request_and_join(host: &mut LobbyState, guest: &mut LobbyState, guest_name: &str) {
        let joined_before = host.hosted_guests().len();
        let host_name = host.my_name().to_string();
        invite_by_name(guest, &host_name);

        for _ in 0..MAX_PUMPS {
            // ホストはリクエストが届いたら許可する。
            let actions: &[InputAction] =
                if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                    &[InputAction::Confirm]
                } else {
                    &[]
                };
            host.update(actions, test_config());
            guest.update(&[], test_config());

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
    /// `first_actions`は最初の1回だけ渡す操作(許可のEnter・開始のTab等)。
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

    /// 同上だが`actions`を毎フレーム渡す。UDPで送る開始要求(#293)が届かなかった場合に
    /// 押し直せるようにするため。
    fn run_until_battle_repeating(
        lobby: &mut LobbyState,
        actions: &[InputAction],
    ) -> Option<Vec<String>> {
        for _ in 0..MAX_PUMPS {
            match lobby.update(actions, test_config()) {
                LobbyOutcome::Battle(state) => return Some(state.player_names.clone()),
                LobbyOutcome::Leave => return None,
                LobbyOutcome::Stay => {}
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// `names[0]`がホスト・以降がゲストとしてロビーを通しで回し、全員の`player_names`を
    /// `names`と同じ順で返す。探索→参加リクエスト→許可→ルーム参加→開始(Tab)→メッシュ
    /// 確立まで実際の通信で進める。
    fn run_room_of(names: &[&str]) -> Vec<Vec<String>> {
        run_room_of_with_ai(names, 0)
    }

    /// `run_room_of`のAIあり版(#300)。ホストは全員が加わった後にAIを`ai_count`人
    /// 追加(I)してから開始する。
    fn run_room_of_with_ai(names: &[&str], ai_count: usize) -> Vec<Vec<String>> {
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

        // 1人ずつリクエストを送り、加わるのを待ってから次へ進む(加わった順がroom内
        // インデックス=`player_names`の並びになる)。
        for (guest, name) in guests.iter_mut().zip(&names[1..]) {
            request_and_join(&mut host, guest, name);
        }
        assert_eq!(host.hosted_guests().len(), names.len() - 1);
        let joined: Vec<&str> = host
            .hosted_guests()
            .iter()
            .map(|guest| guest.name())
            .collect();
        assert_eq!(
            joined,
            names[1..],
            "迎え入れた順はリクエストを送った順のはず"
        );

        // AIの枠(#300)は全員が加わった後に増やす(ゲストを迎える途中で`Discovering`を
        // 抜けるため、その間に増やしても0へ戻る)。
        for _ in 0..ai_count {
            host.update(&[InputAction::IncreaseRoomAiCount], test_config());
        }
        assert_eq!(
            host.room_ai_count(),
            ai_count,
            "前提: ホストはAIを{ai_count}人ぶん追加できているはず"
        );

        // 開始操作(Tab)はメッシュ確立までブロックするため、ゲスト側は別スレッドで回す。
        let guest_threads: Vec<_> = guests
            .into_iter()
            .map(|mut guest| thread::spawn(move || run_until_battle(&mut guest, &[])))
            .collect();
        let host_names = run_until_battle(&mut host, &[InputAction::StartRoom]);

        let mut all = vec![host_names.expect("ホストは対戦を開始するはず")];
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
        // Confirmしても新たなリクエストは送らない(誤操作で5人目を誘えてしまうのを防ぐ)。
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
            ai_count: 0,
        });

        lobby.update(&[InputAction::Confirm], test_config());

        assert!(
            matches!(lobby.phase(), LobbyPhase::Discovering { guests, .. } if guests.len() == 3),
            "上限に達している間はConfirmしても応答待ちへ移らないはず"
        );
    }

    #[test]
    fn pressing_the_ai_battle_key_asks_how_many_opponents_to_face() {
        // #296: 相手が1人も見つかっていなくてもVで人数選択へ入れる(通信を使わないため)。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        let outcome = lobby.update(&[InputAction::StartAiBattle], test_config());

        assert!(matches!(outcome, LobbyOutcome::Stay));
        assert!(
            matches!(
                lobby.phase(),
                LobbyPhase::SelectingAiOpponentCount { ai_count: 1 }
            ),
            "Vを押したらAIの人数選択(初期値1)へ移るはず"
        );
    }

    #[test]
    fn the_ai_opponent_count_stays_within_the_supported_range() {
        // #296: 合計人数がROOM_MAX_PLAYERS(4人)を超えないよう、AIは1〜3人に収める。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.set_phase(LobbyPhase::SelectingAiOpponentCount { ai_count: 1 });

        // 上限を超えて増やそうとしても3で止まる。
        for _ in 0..5 {
            lobby.update(&[InputAction::FaceUp], test_config());
        }
        assert!(
            matches!(
                lobby.phase(),
                LobbyPhase::SelectingAiOpponentCount { ai_count: 3 }
            ),
            "AIの人数は3人で止まるはず"
        );

        // 下限も同じく1で止まる(0人=1人対戦にはならない)。
        for _ in 0..5 {
            lobby.update(&[InputAction::FaceDown], test_config());
        }
        assert!(
            matches!(
                lobby.phase(),
                LobbyPhase::SelectingAiOpponentCount { ai_count: 1 }
            ),
            "AIの人数は1人で止まるはず"
        );
    }

    #[test]
    fn confirming_the_ai_opponent_count_starts_a_battle_with_that_many_opponents() {
        // #296: AI2人を選んでEnter→自分+AI2人の3人で対戦が始まる。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.set_phase(LobbyPhase::SelectingAiOpponentCount { ai_count: 2 });

        let outcome = lobby.update(&[InputAction::Confirm], test_config());

        let LobbyOutcome::Battle(state) = outcome else {
            panic!("AI対戦が始まるはず");
        };
        assert_eq!(state.games.len(), 3, "自分+AI2人ぶんの盤面があるはず");
        assert_eq!(state.player_names.len(), 3);
        assert_eq!(state.player_names[0], "me");
    }

    #[test]
    fn escaping_the_ai_opponent_count_goes_back_to_discovering() {
        // #296: 人数選択をやめたら探索へ戻る(タイトルまで戻したりはしない)。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.set_phase(LobbyPhase::SelectingAiOpponentCount { ai_count: 2 });

        let outcome = lobby.update(&[InputAction::Quit], test_config());

        assert!(matches!(outcome, LobbyOutcome::Stay));
        assert!(matches!(lobby.phase(), LobbyPhase::Discovering { .. }));
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
        // #293: 申し込んだ側は応答待ち、受けた側は許可するか聞く画面へ移る。
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);

        guest.update(&[InputAction::Confirm], test_config());
        assert!(
            matches!(guest.phase(), LobbyPhase::AwaitingInviteResponse { target, .. } if target.player_name == "host"),
            "申し込んだ側は応答待ちへ移るはず"
        );

        for _ in 0..200 {
            host.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(host.phase(), LobbyPhase::IncomingInvite { from, pending, .. } if from.player_name == "guest" && pending.is_empty()),
            "申し込まれた側は許可を聞く画面へ移るはず"
        );
    }

    #[test]
    fn declining_an_invite_sends_both_sides_back_toward_discovering() {
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);

        guest.update(&[InputAction::Confirm], test_config());
        for _ in 0..200 {
            host.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(host.phase(), LobbyPhase::IncomingInvite { .. }));

        // Escで拒否する。拒否した側はすぐ探索へ戻り、申し込んだ側は通知を経て戻る。
        host.update(&[InputAction::Quit], test_config());
        assert!(matches!(host.phase(), LobbyPhase::Discovering { .. }));

        for _ in 0..200 {
            guest.update(&[], test_config());
            if matches!(guest.phase(), LobbyPhase::Notice { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(guest.phase(), LobbyPhase::Notice { message, .. } if message == "相手に断られました"),
            "断られたことが通知として出るはず"
        );
    }

    #[test]
    fn an_invite_that_is_never_answered_times_out_into_a_notice() {
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);
        guest.update(&[InputAction::Confirm], test_config());

        // 実時間で10秒待つ代わりに、送信時刻をタイムアウトぶん過去へ倒す。
        let target = match &guest.phase {
            LobbyPhase::AwaitingInviteResponse { target, .. } => target.clone(),
            _ => panic!("前提: 応答待ちのはず"),
        };
        guest.phase = LobbyPhase::AwaitingInviteResponse {
            target,
            sent_at: Instant::now() - Duration::from_millis(INVITE_TIMEOUT_MS),
            guests: Vec::new(),
        };
        guest.update(&[], test_config());

        assert!(
            matches!(guest.phase(), LobbyPhase::Notice { message, .. } if message == "応答がありませんでした")
        );

        // 受けた側は放置しただけなので、探索を続けている。
        assert!(matches!(host.phase(), LobbyPhase::Discovering { .. }));
    }

    #[test]
    fn a_join_request_that_arrives_once_the_room_is_full_is_declined_automatically() {
        // #293: ゲストがいてもリクエストは受け付けるが、上限人数(`ROOM_MAX_PLAYERS`)に
        // 達している間は自動で断る(無視すると相手がタイムアウトまで待たされる)。
        let (mut host, mut other) = facing_lobbies();
        discover_each_other(&mut host, &mut other);
        host.set_phase(LobbyPhase::Discovering {
            guests: vec![
                HostedGuest::for_test("g1"),
                HostedGuest::for_test("g2"),
                HostedGuest::for_test("g3"),
            ],
            ai_count: 0,
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
            matches!(host.phase(), LobbyPhase::Discovering { guests, .. } if guests.len() == 3),
            "満員のホストは確認画面へ移らず、集めたルームを保ったままのはず"
        );
        assert!(
            matches!(other.phase(), LobbyPhase::Notice { message, .. } if message == "相手に断られました"),
            "申し込んだ側には断られた通知が出るはず"
        );
    }

    #[test]
    fn a_join_request_that_arrives_while_a_guest_is_already_hosted_is_still_accepted() {
        // #293: ゲストを1人迎えた後でも、上限に達していなければ確認画面へ移る
        // (#276の「ゲストがいれば自動で断る」動作はここで無くなった)。
        let (mut host, mut other) = facing_lobbies();
        discover_each_other(&mut host, &mut other);
        host.set_phase(LobbyPhase::Discovering {
            guests: vec![HostedGuest::for_test("joined")],
            ai_count: 0,
        });

        invite_by_name(&mut other, "host");
        for _ in 0..200 {
            host.update(&[], test_config());
            other.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        assert!(
            matches!(host.phase(), LobbyPhase::IncomingInvite { guests, .. } if guests.len() == 1),
            "迎え入れ済みのゲストを保ったまま確認画面へ移るはず"
        );
        assert!(
            matches!(other.phase(), LobbyPhase::AwaitingInviteResponse { .. }),
            "申し込んだ側は断られず応答待ちのままのはず"
        );
    }

    #[test]
    fn a_second_invite_that_arrives_while_deciding_on_the_first_is_queued_and_shown_next() {
        // #291で発覚: 2人から同時に申し込まれると、先着以外は無視されタイムアウト
        // (`INVITE_TIMEOUT_MS`)まで無応答で待たされてしまっていた。#293では順番待ちへ
        // 積み、1件目を処理した後に続けて確認する。
        let mut lobbies = facing_lobbies_of(&["host", "guest-a", "guest-b"]);
        discover_all(&mut lobbies);
        let (host, rest) = lobbies.split_first_mut().unwrap();
        let (guest_a, rest) = rest.split_first_mut().unwrap();
        let guest_b = &mut rest[0];

        invite_by_name(guest_a, "host");
        invite_by_name(guest_b, "host");

        let mut first = None;
        for _ in 0..200 {
            host.update(&[], test_config());
            guest_a.update(&[], test_config());
            guest_b.update(&[], test_config());
            if let LobbyPhase::IncomingInvite { from, pending, .. } = host.phase()
                && pending.len() == 1
            {
                first = Some(from.player_name.clone());
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let first = first.expect("2件目は順番待ちへ積まれるはず");

        // どちらも断られていない(タイムアウト待ちにもなっていない)。
        for (guest, name) in [(&*guest_a, "guest-a"), (&*guest_b, "guest-b")] {
            assert!(
                matches!(guest.phase(), LobbyPhase::AwaitingInviteResponse { .. }),
                "{name}は応答待ちのままのはず"
            );
        }

        // 1件目を断ると、順番待ちの1件が次の確認として出る。
        host.update(&[InputAction::Quit], test_config());
        let LobbyPhase::IncomingInvite { from, pending, .. } = host.phase() else {
            panic!("順番待ちの1件が次の確認画面になるはず");
        };
        assert_ne!(from.player_name, first, "次に出るのは2件目のはず");
        assert!(pending.is_empty(), "順番待ちは空になるはず");
    }

    #[test]
    fn a_notice_returns_to_discovering_once_it_has_been_shown_long_enough() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.phase = LobbyPhase::Notice {
            message: "テスト".to_string(),
            shown_at: Instant::now() - Duration::from_millis(NOTICE_DISPLAY_MS),
            guests: Vec::new(),
            pending: Vec::new(),
        };

        lobby.update(&[], test_config());

        assert!(matches!(lobby.phase(), LobbyPhase::Discovering { .. }));
    }

    #[test]
    fn a_notice_hands_a_queued_request_over_as_the_next_one_to_decide_on() {
        // #293: 通知表示中に届いたリクエストは、通知を抜けた後に確認画面として出す。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        let waiting = DiscoveredPeer::for_test(
            "waiting",
            std::net::IpAddr::from(Ipv4Addr::new(192, 168, 0, 8)),
            39398,
        );
        lobby.phase = LobbyPhase::Notice {
            message: "テスト".to_string(),
            shown_at: Instant::now() - Duration::from_millis(NOTICE_DISPLAY_MS),
            guests: Vec::new(),
            pending: vec![waiting],
        };

        lobby.update(&[], test_config());

        assert!(
            matches!(lobby.phase(), LobbyPhase::IncomingInvite { from, pending, .. } if from.player_name == "waiting" && pending.is_empty()),
            "通知を抜けた後、順番待ちの1件が確認画面になるはず"
        );
    }

    #[test]
    fn a_notice_carries_the_already_hosted_guests_back_to_discovering() {
        // #284: リクエストの失敗・タイムアウト・接続失敗1つで、既に迎え入れていた
        // ゲストまでルームから失われないようにする。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.phase = LobbyPhase::Notice {
            message: "テスト".to_string(),
            shown_at: Instant::now() - Duration::from_millis(NOTICE_DISPLAY_MS),
            guests: vec![HostedGuest::for_test("already-joined")],
            pending: Vec::new(),
        };

        lobby.update(&[], test_config());

        assert!(
            matches!(lobby.phase(), LobbyPhase::Discovering { guests, .. } if guests.len() == 1 && guests[0].name() == "already-joined"),
            "通知を抜けた後も既に迎えていたゲストが残っているはず"
        );
    }

    #[test]
    fn accepting_an_invite_connects_both_sides_and_starts_a_battle() {
        // 探索→参加リクエスト→許可→ルーム参加→開始(Tab)→対戦成立までを通しで確認する。
        // #293では申し込んだ側がゲスト(クライアント役)・受けた側がホスト(サーバ役)。
        let names = run_room_of(&["host", "guest"]);

        assert_eq!(
            names[0],
            vec!["host".to_string(), "guest".to_string()],
            "ホストから見た並びはindex 0が自分・1がゲスト"
        );
        assert_eq!(
            names[1],
            vec!["guest".to_string(), "host".to_string()],
            "ゲストから見た並びもindex 0が自分・1がホスト"
        );
    }

    #[test]
    fn a_room_of_three_players_starts_a_battle_for_everyone() {
        // #276: ゲスト2人が順にリクエストを送り、ホストが開始する。全員のindex 0が
        // 自分で、残りはroom内インデックス順(ホスト→ゲスト1→ゲスト2から自分を除いたもの)。
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

    #[test]
    fn a_guest_can_start_the_battle_without_the_host_confirming() {
        // #293: 開始はゲストからも出せる。ホストは何も操作しないまま対戦へ入る。
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);
        request_and_join(&mut host, &mut guest, "guest");

        // ゲスト側はメッシュ確立まで待ち合わせるため別スレッドで回す。
        let guest_thread = thread::spawn(move || {
            let mut guest = guest;
            run_until_battle_repeating(&mut guest, &[InputAction::StartRoom])
        });
        let host_names = run_until_battle(&mut host, &[]);

        assert_eq!(
            host_names.expect("ホストはゲストの開始要求で対戦へ入るはず"),
            vec!["host".to_string(), "guest".to_string()]
        );
        let guest_names = guest_thread
            .join()
            .expect("ゲストのスレッドは正常終了するはず")
            .expect("ゲストも対戦へ入るはず");
        assert_eq!(guest_names, vec!["guest".to_string(), "host".to_string()]);
    }

    #[test]
    fn a_start_request_is_ignored_while_the_host_has_no_guest() {
        // #293: ゲストが1人もいないホストへ開始要求が届いても、1人では始められない
        // ので無視する。
        let (mut host, mut guest) = facing_lobbies();
        discover_each_other(&mut host, &mut guest);
        let host_peer = guest
            .peers()
            .iter()
            .find(|peer| peer.player_name == "host")
            .expect("候補にhostがいるはず")
            .clone();
        // 結果を送る側(`_result_tx`)を残しておかないと、開始待ちが失敗扱いになる。
        let (_result_tx, result_rx) = mpsc::channel();
        guest.set_phase(LobbyPhase::WaitingForRoomStart {
            result_rx,
            host_peer,
        });

        guest.update(&[InputAction::StartRoom], test_config());
        for _ in 0..50 {
            assert!(matches!(
                host.update(&[], test_config()),
                LobbyOutcome::Stay
            ));
            thread::sleep(Duration::from_millis(2));
        }

        assert!(
            matches!(host.phase(), LobbyPhase::Discovering { guests, .. } if guests.is_empty()),
            "開始要求は無視され、探索を続けているはず"
        );
    }

    #[test]
    fn join_requests_from_several_players_are_handled_one_at_a_time() {
        // #293: 同時に届いた複数のリクエストを1件ずつ許可して、全員を迎え入れられる
        // (#291のように無視されて待たされる参加者を出さない)。
        let mut lobbies = facing_lobbies_of(&["host", "guest-a", "guest-b"]);
        discover_all(&mut lobbies);
        let (host, rest) = lobbies.split_first_mut().unwrap();
        let (guest_a, rest) = rest.split_first_mut().unwrap();
        let guest_b = &mut rest[0];

        invite_by_name(guest_a, "host");
        invite_by_name(guest_b, "host");

        for _ in 0..MAX_PUMPS {
            // ホストは確認画面が出るたびに許可する(1件ずつしか出ない)。
            let actions: &[InputAction] =
                if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                    &[InputAction::Confirm]
                } else {
                    &[]
                };
            host.update(actions, test_config());
            guest_a.update(&[], test_config());
            guest_b.update(&[], test_config());
            if host.hosted_guests().len() == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        let mut joined: Vec<&str> = host
            .hosted_guests()
            .iter()
            .map(|guest| guest.name())
            .collect();
        joined.sort_unstable();
        assert_eq!(
            joined,
            vec!["guest-a", "guest-b"],
            "2人とも迎え入れられるはず"
        );
        for (guest, name) in [(&*guest_a, "guest-a"), (&*guest_b, "guest-b")] {
            assert!(
                matches!(guest.phase(), LobbyPhase::WaitingForRoomStart { .. }),
                "{name}はルームへ加わって開始待ちのはず"
            );
        }
    }

    #[test]
    fn queued_join_requests_are_declined_once_accepting_the_earlier_ones_fills_the_room() {
        // 1件ずつ許可していく途中で満員(`ROOM_MAX_PLAYERS`)に達したら、順番待ちの
        // 残りは`IncomingInvite`として提示されず、即座に断られるはず(back_to_discovering
        // が確認済みの人数で毎回判定するため、受信時点では入れる余地があった相手も
        // 許可を重ねるうちに超過してしまう問題への対応)。
        let mut lobbies = facing_lobbies_of(&["host", "guest-a", "guest-b"]);
        discover_all(&mut lobbies);
        let (host, rest) = lobbies.split_first_mut().unwrap();
        let (guest_a, rest) = rest.split_first_mut().unwrap();
        let guest_b = &mut rest[0];

        // 既に2人迎えている(自分含め3人)。ROOM_MAX_PLAYERS=4なので、あと1人だけ入れる。
        host.set_phase(LobbyPhase::Discovering {
            guests: vec![
                HostedGuest::for_test("already-1"),
                HostedGuest::for_test("already-2"),
            ],
            ai_count: 0,
        });

        invite_by_name(guest_a, "host");
        invite_by_name(guest_b, "host");

        for _ in 0..MAX_PUMPS {
            let actions: &[InputAction] =
                if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                    &[InputAction::Confirm]
                } else {
                    &[]
                };
            host.update(actions, test_config());
            guest_a.update(&[], test_config());
            guest_b.update(&[], test_config());
            if matches!(guest_a.phase(), LobbyPhase::Notice { .. })
                || matches!(guest_b.phase(), LobbyPhase::Notice { .. })
            {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(
            host.hosted_guests().len(),
            3,
            "満員(自分含め4人)を超えて迎え入れてはいないはず"
        );
        let (accepted, declined) = if matches!(guest_a.phase(), LobbyPhase::Notice { .. }) {
            (guest_b, guest_a)
        } else {
            (guest_a, guest_b)
        };
        assert!(
            matches!(accepted.phase(), LobbyPhase::WaitingForRoomStart { .. }),
            "先に許可された側はルームへ加わっているはず"
        );
        assert!(
            matches!(declined.phase(), LobbyPhase::Notice { message, .. } if message == "相手に断られました"),
            "満員後に順番が回ってきた側は断られるはず"
        );
    }

    // -----------------------------------------------------------------------
    // ルームへ混ぜるAI(#300)。#296のAI対戦(V)とは別で、通信ありのルームに
    // 「接続を持たない参加者」を足す。
    // -----------------------------------------------------------------------

    #[test]
    fn a_room_starts_with_no_ai_and_the_keys_add_and_remove_one_at_a_time() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.set_phase(LobbyPhase::Discovering {
            guests: vec![HostedGuest::for_test("g1")],
            ai_count: 0,
        });
        assert_eq!(lobby.room_ai_count(), 0, "初期値はAIなし");

        lobby.update(&[InputAction::IncreaseRoomAiCount], test_config());
        assert_eq!(lobby.room_ai_count(), 1);

        lobby.update(&[InputAction::IncreaseRoomAiCount], test_config());
        assert_eq!(lobby.room_ai_count(), 2, "自分+ゲスト1人+AI2人=4人まで");

        lobby.update(&[InputAction::DecreaseRoomAiCount], test_config());
        assert_eq!(lobby.room_ai_count(), 1);

        lobby.update(&[InputAction::DecreaseRoomAiCount], test_config());
        lobby.update(&[InputAction::DecreaseRoomAiCount], test_config());
        assert_eq!(lobby.room_ai_count(), 0, "0より下へは減らないはず");
    }

    #[test]
    fn the_ai_count_stops_so_that_the_room_stays_within_its_capacity() {
        // 自分+ゲスト+AIが`ROOM_MAX_PLAYERS`(4人)に収まる人数で止まる。
        for guest_count in 0..ROOM_MAX_PLAYERS {
            let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
            let guests = (0..guest_count)
                .map(|index| HostedGuest::for_test(&format!("g{index}")))
                .collect();
            lobby.set_phase(LobbyPhase::Discovering {
                guests,
                ai_count: 0,
            });

            for _ in 0..ROOM_MAX_PLAYERS + 1 {
                lobby.update(&[InputAction::IncreaseRoomAiCount], test_config());
            }

            assert_eq!(
                lobby.room_ai_count(),
                ROOM_MAX_PLAYERS - 1 - guest_count,
                "ゲスト{guest_count}人なら残り枠ぶんまでしか増えないはず"
            );
        }
    }

    #[test]
    fn the_ai_count_keys_do_nothing_outside_the_room() {
        // AIの枠を持つのは`Discovering`だけ。#296の人数選択(V)の値を巻き込まないこと。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.set_phase(LobbyPhase::SelectingAiOpponentCount { ai_count: 1 });

        lobby.update(&[InputAction::IncreaseRoomAiCount], test_config());

        assert!(
            matches!(
                lobby.phase(),
                LobbyPhase::SelectingAiOpponentCount { ai_count: 1 }
            ),
            "#296の人数選択は動かないはず"
        );
        assert_eq!(lobby.room_ai_count(), 0, "ルームのAIの枠も0のままのはず");
    }

    #[test]
    fn a_room_with_an_ai_lists_it_after_the_guests_for_everyone() {
        // ホスト+ゲスト1人+AI1人。AIはrosterの末尾に並び、ゲストからも同じ名前で見える
        // (代理送信のroom内インデックスが全員で一致している必要があるため)。
        let names = run_room_of_with_ai(&["host", "guest"], 1);

        assert_eq!(
            names[0],
            vec![
                "host".to_string(),
                "guest".to_string(),
                room::ai_member_name(1)
            ]
        );
        assert_eq!(
            names[1],
            vec![
                "guest".to_string(),
                "host".to_string(),
                room::ai_member_name(1)
            ]
        );
    }

    #[test]
    fn a_room_can_be_filled_up_with_more_than_one_ai() {
        // ホスト+ゲスト1人+AI2人で`ROOM_MAX_PLAYERS`(4人)ぴったり。
        let names = run_room_of_with_ai(&["host", "guest"], 2);

        assert_eq!(
            names[0],
            vec![
                "host".to_string(),
                "guest".to_string(),
                room::ai_member_name(1),
                room::ai_member_name(2)
            ]
        );
        assert_eq!(
            names[1],
            vec![
                "guest".to_string(),
                "host".to_string(),
                room::ai_member_name(1),
                room::ai_member_name(2)
            ]
        );
    }
}
