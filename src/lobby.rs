//! 対戦相手を探して招待をやり取りするロビー画面の状態(#256。spec.md 12.1)。
//!
//! UDP探索(`discovery.rs`)の候補リストと、招待→TCP接続→ハンドシェイク(`net.rs`)→
//! 対戦開始(`battle.rs`)までの流れを1つの状態機械にまとめる。入力の取り込みと描画は
//! 画面側(`app::screens::tick_network_lobby`・`ui::render::draw_network_lobby`)が行い、
//! ここは「押された操作」と「経過時間」を受け取ってフェーズを進めることに専念する
//! (ターミナルを持たずに結合テストできるようにするため)。

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::battle::BattleState;
use crate::discovery::{DiscoveredPeer, Discovery};
use crate::game::InputAction;
use crate::net::{
    self, BattleConfig, DEFAULT_TCP_PORT, DiscoveryPacket, INVITE_TIMEOUT_MS, PacketType,
    TCP_CONNECT_TIMEOUT_MS,
};

/// 短い通知(拒否・タイムアウト・接続失敗)を表示し続ける時間(ms)。この後は自動的に
/// 探索へ戻る。
const NOTICE_DISPLAY_MS: u64 = 1500;

/// 対戦用TCPポートの空きを探す個数。既定値から1つずつ上へ試す(spec.md 12.1
/// 「使用中なら39395, 39396…」)。
const TCP_PORT_SEARCH_COUNT: u16 = 16;

/// 接続に失敗したときの通知文。ホスト役・クライアント役の双方で使う。
const CONNECT_FAILED_MESSAGE: &str = "接続できませんでした";

/// ロビーの状態。
pub struct LobbyState {
    discovery: Discovery,
    /// 対戦用のTCP listener。ロビーにいる間はずっとlistenしておき、このポートを
    /// HELLOで広告し続ける(招待を受けた時点で改めてbindすると、広告済みのポートが
    /// 空いている保証が無い)。非ブロッキングに設定済み。
    listener: TcpListener,
    /// 自分の表示名。HELLOの広告にも、TCPハンドシェイクの`Hello`にも使う。
    my_name: String,
    /// 候補リスト上のカーソル位置。
    selection: usize,
    phase: LobbyPhase,
}

/// ロビーの進行段階。
pub enum LobbyPhase {
    /// 候補を探しながら招待を待っている通常状態。
    Discovering,
    /// 自分から招待を送り、相手の応答を待っている。
    AwaitingInviteResponse {
        target: DiscoveredPeer,
        sent_at: Instant,
    },
    /// 招待を受け取り、承諾するか聞いている。
    IncomingInvite { from: DiscoveredPeer },
    /// ACCEPTを送った後、TCPサーバ役として相手のconnectを待っている。
    ConnectingAsHost {
        opponent_name: String,
        started: Instant,
    },
    /// ACCEPTを受け取った後、TCPクライアント役としてconnectを試みる。
    ConnectingAsClient {
        addr: SocketAddr,
        opponent_name: String,
    },
    /// 拒否・タイムアウト・接続失敗を短く伝える。時間が経つと探索へ戻る。
    Notice { message: String, shown_at: Instant },
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

impl LobbyState {
    /// 探索用UDPソケットと対戦用TCP listenerを確保してロビーを開始する。
    /// どちらかのポートが取れない場合はエラーを返す(呼び出し元はタイトルへ留まる)。
    pub fn new(my_name: String) -> io::Result<Self> {
        let listener = bind_battle_listener()?;
        listener.set_nonblocking(true)?;
        let tcp_port = listener.local_addr()?.port();
        let discovery = Discovery::start(my_name.clone(), tcp_port)?;
        Ok(Self::new_with(discovery, listener, my_name))
    }

    fn new_with(discovery: Discovery, listener: TcpListener, my_name: String) -> Self {
        Self {
            discovery,
            listener,
            my_name,
            selection: 0,
            phase: LobbyPhase::Discovering,
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

    /// 1フレーム分進める。`actions`はこのフレームに届いた操作、`config`は自分が
    /// ホスト役になった場合に相手へ強制適用する設定(spec.md 12.2)。
    pub fn update(&mut self, actions: &[InputAction], config: BattleConfig) -> LobbyOutcome {
        let packets = self.discovery.tick();
        self.apply_packets(packets);
        self.clamp_selection();

        for &action in actions {
            if let Some(outcome) = self.apply_action(action) {
                return outcome;
            }
        }

        self.advance_phase(config)
    }

    /// 受信した自分宛のINVITE/ACCEPT/DECLINEを、現在のフェーズに応じて反映する。
    fn apply_packets(&mut self, packets: Vec<DiscoveryPacket>) {
        for packet in packets {
            let next = match (&self.phase, packet.packet_type) {
                // 招待された。送り主は候補リストに載っているはず(HELLOを1秒間隔で
                // 流し合っているため)で、載っていなければ返信先が分からないので無視する。
                (LobbyPhase::Discovering, PacketType::Invite) => self
                    .discovery
                    .peers()
                    .iter()
                    .find(|peer| peer.sender_id == packet.sender_id)
                    .map(|peer| LobbyPhase::IncomingInvite { from: peer.clone() }),
                (LobbyPhase::AwaitingInviteResponse { target, .. }, PacketType::Accept)
                    if target.sender_id == packet.sender_id =>
                {
                    // 接続先は相手のIPと、ACCEPTが載せてきた最新のTCPポート。
                    // 招待側がクライアント役になる(spec.md 12.1)。
                    Some(LobbyPhase::ConnectingAsClient {
                        addr: SocketAddr::new(target.addr, packet.tcp_port),
                        opponent_name: target.player_name.clone(),
                    })
                }
                (LobbyPhase::AwaitingInviteResponse { target, .. }, PacketType::Decline)
                    if target.sender_id == packet.sender_id =>
                {
                    Some(notice("相手に断られました"))
                }
                _ => None,
            };

            if let Some(phase) = next {
                // 接続へ移る側は、この時点で募集を止める(spec.md 12.1
                // 「ACCEPTを受信したら、UDPブロードキャスト送信を停止し」)。
                if matches!(phase, LobbyPhase::ConnectingAsClient { .. }) {
                    self.discovery.send_bye();
                }
                self.phase = phase;
            }
        }
    }

    /// 操作を1つ反映する。ロビーを抜ける場合だけ`Some`を返す。
    fn apply_action(&mut self, action: InputAction) -> Option<LobbyOutcome> {
        match &self.phase {
            LobbyPhase::Discovering => match action {
                InputAction::FaceUp => self.move_selection(false),
                InputAction::FaceDown => self.move_selection(true),
                InputAction::Confirm => self.invite_selected(),
                InputAction::Quit => {
                    // 相手の候補リストから即座に消えるよう、抜ける前にBYEを流す。
                    self.discovery.send_bye();
                    return Some(LobbyOutcome::Leave);
                }
                _ => {}
            },
            // 招待のキャンセル。探索自体は続けるためBYEは送らない。
            LobbyPhase::AwaitingInviteResponse { .. } => {
                if action == InputAction::Quit {
                    self.phase = LobbyPhase::Discovering;
                }
            }
            LobbyPhase::IncomingInvite { from } => match action {
                // Enter=承諾、Esc=拒否(既存の入力体系に合わせる。設計書4節)。
                InputAction::Confirm => {
                    let opponent = from.clone();
                    if self.discovery.send_accept(&opponent).is_err() {
                        self.phase = notice(CONNECT_FAILED_MESSAGE);
                        return None;
                    }
                    // 承諾した側がTCPサーバ役になる(spec.md 12.1)。listenは
                    // ロビー開始時から続けているので、あとはacceptを待つだけ。
                    self.phase = LobbyPhase::ConnectingAsHost {
                        opponent_name: opponent.player_name,
                        started: Instant::now(),
                    };
                }
                InputAction::Quit => {
                    let inviter = from.clone();
                    let _ = self.discovery.send_decline(&inviter);
                    self.phase = LobbyPhase::Discovering;
                }
                _ => {}
            },
            // 接続中・通知表示中は短い区間なので操作を受け付けない。
            LobbyPhase::ConnectingAsHost { .. }
            | LobbyPhase::ConnectingAsClient { .. }
            | LobbyPhase::Notice { .. } => {}
        }

        None
    }

    /// 時間経過・接続の進行によるフェーズの更新。
    fn advance_phase(&mut self, config: BattleConfig) -> LobbyOutcome {
        match &self.phase {
            // 招待を受けた側には期限を設けない(招待した側が`INVITE_TIMEOUT_MS`で
            // 諦めるため、両側に時計を持たせる必要は無い)。
            LobbyPhase::Discovering | LobbyPhase::IncomingInvite { .. } => LobbyOutcome::Stay,
            LobbyPhase::AwaitingInviteResponse { sent_at, .. } => {
                if sent_at.elapsed() >= Duration::from_millis(INVITE_TIMEOUT_MS) {
                    self.phase = notice("応答がありませんでした");
                }
                LobbyOutcome::Stay
            }
            LobbyPhase::ConnectingAsHost { started, .. } => {
                let started = *started;
                self.accept_opponent(started, config)
            }
            LobbyPhase::ConnectingAsClient { addr, .. } => {
                let addr = *addr;
                self.connect_to_opponent(addr)
            }
            LobbyPhase::Notice { shown_at, .. } => {
                if shown_at.elapsed() >= Duration::from_millis(NOTICE_DISPLAY_MS) {
                    self.phase = LobbyPhase::Discovering;
                }
                LobbyOutcome::Stay
            }
        }
    }

    /// ホスト役として相手のconnectを待つ(非ブロッキングのため毎フレーム1回試す)。
    fn accept_opponent(&mut self, started: Instant, config: BattleConfig) -> LobbyOutcome {
        match self.listener.accept() {
            Ok((stream, _)) => {
                // 接続できたので募集からは抜ける(spec.md 12.1)。
                self.discovery.send_bye();
                match host_handshake(stream, &self.my_name, config) {
                    Ok(state) => LobbyOutcome::Battle(Box::new(state)),
                    Err(_) => {
                        self.phase = notice(CONNECT_FAILED_MESSAGE);
                        LobbyOutcome::Stay
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= Duration::from_millis(TCP_CONNECT_TIMEOUT_MS) {
                    self.phase = notice(CONNECT_FAILED_MESSAGE);
                }
                LobbyOutcome::Stay
            }
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE);
                LobbyOutcome::Stay
            }
        }
    }

    /// クライアント役として相手へ接続する。`connect_timeout`自体が待ち時間を持つため
    /// 1回で決着させる(この間フレームが止まるが、対戦成立までの一度きりの処理)。
    fn connect_to_opponent(&mut self, addr: SocketAddr) -> LobbyOutcome {
        match client_handshake(addr, &self.my_name) {
            Ok(state) => LobbyOutcome::Battle(Box::new(state)),
            Err(_) => {
                self.phase = notice(CONNECT_FAILED_MESSAGE);
                LobbyOutcome::Stay
            }
        }
    }

    /// 選択中の候補へ招待を送る。候補が1件も無ければ何もしない。
    fn invite_selected(&mut self) {
        let Some(target) = self.discovery.peers().get(self.selection).cloned() else {
            return;
        };
        if self.discovery.send_invite(&target).is_err() {
            // 送れなかった場合は探索を続ける(相手が既にいなくなった等)。
            return;
        }
        self.phase = LobbyPhase::AwaitingInviteResponse {
            target,
            sent_at: Instant::now(),
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

/// 短い通知フェーズを作る。
fn notice(message: &str) -> LobbyPhase {
    LobbyPhase::Notice {
        message: message.to_string(),
        shown_at: Instant::now(),
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

/// 受理した接続でホスト役のハンドシェイクを行い、対戦状態を組み立てる。
fn host_handshake(
    mut stream: TcpStream,
    my_name: &str,
    config: BattleConfig,
) -> io::Result<BattleState> {
    // listenerが非ブロッキングのため、環境によっては受理したストリームもそれを
    // 引き継ぐ。ハンドシェイク以降はブロッキング前提なので明示的に戻す。
    stream.set_nonblocking(false)?;
    let handshake = net::run_host_handshake(&mut stream, my_name, config)?;
    BattleState::from_handshake(handshake, stream)
}

/// 相手へ接続してクライアント役のハンドシェイクを行い、対戦状態を組み立てる。
fn client_handshake(addr: SocketAddr, my_name: &str) -> io::Result<BattleState> {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_millis(TCP_CONNECT_TIMEOUT_MS))?;
    let handshake = net::run_client_handshake(&mut stream, my_name)?;
    BattleState::from_handshake(handshake, stream)
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
        let discovery = Discovery::start_on_loopback(my_name.clone(), tcp_port)?;
        Ok(Self::new_with(discovery, listener, my_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use std::thread;

    /// テスト用の対戦設定。盤面生成を軽くするため短いコース(20m)にする。
    fn test_config() -> BattleConfig {
        BattleConfig::from_settings(&Settings::default(), 20)
    }

    /// 互いのHELLOが届くよう向かい合わせた2つのロビー。
    fn facing_lobbies() -> (LobbyState, LobbyState) {
        let mut host = LobbyState::new_on_loopback("host".to_string()).unwrap();
        let mut client = LobbyState::new_on_loopback("client".to_string()).unwrap();
        let host_addr = host.discovery.local_addr();
        let client_addr = client.discovery.local_addr();
        host.discovery.set_hello_target(client_addr);
        client.discovery.set_hello_target(host_addr);
        (host, client)
    }

    /// 相手を候補リストに載せるまで、両者のHELLOを流し合う。
    fn discover_each_other(host: &mut LobbyState, client: &mut LobbyState) {
        for _ in 0..200 {
            // 実運用の1秒間隔を実時間で待つとテストが遅くなるため、毎回送らせる。
            host.discovery.resend_hello_now();
            client.discovery.resend_hello_now();
            host.update(&[], test_config());
            client.update(&[], test_config());
            if !host.peers().is_empty() && !client.peers().is_empty() {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_lobby_starts_out_discovering_with_no_candidates() {
        let lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        assert!(matches!(lobby.phase(), LobbyPhase::Discovering));
        assert!(lobby.peers().is_empty());
        assert_eq!(lobby.selection(), 0);
    }

    #[test]
    fn confirming_without_any_candidate_keeps_the_lobby_discovering() {
        // 候補が1件も無い状態でEnterを押しても、送る相手がいないので何も起きない。
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        let outcome = lobby.update(&[InputAction::Confirm], test_config());

        assert!(matches!(outcome, LobbyOutcome::Stay));
        assert!(matches!(lobby.phase(), LobbyPhase::Discovering));
    }

    #[test]
    fn escape_while_discovering_leaves_the_lobby() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();

        let outcome = lobby.update(&[InputAction::Quit], test_config());

        assert!(matches!(outcome, LobbyOutcome::Leave));
    }

    #[test]
    fn the_selection_cycles_through_the_candidates_in_both_directions() {
        let (mut host, mut client) = facing_lobbies();
        discover_each_other(&mut host, &mut client);
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
        let (mut host, mut client) = facing_lobbies();
        discover_each_other(&mut host, &mut client);

        client.update(&[InputAction::Confirm], test_config());
        assert!(
            matches!(client.phase(), LobbyPhase::AwaitingInviteResponse { target, .. } if target.player_name == "host"),
            "招待した側は応答待ちへ移るはず"
        );

        for _ in 0..200 {
            host.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(host.phase(), LobbyPhase::IncomingInvite { from } if from.player_name == "client"),
            "招待された側は承諾を聞く画面へ移るはず"
        );
    }

    #[test]
    fn declining_an_invite_sends_both_sides_back_toward_discovering() {
        let (mut host, mut client) = facing_lobbies();
        discover_each_other(&mut host, &mut client);

        client.update(&[InputAction::Confirm], test_config());
        for _ in 0..200 {
            host.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(host.phase(), LobbyPhase::IncomingInvite { .. }));

        // Escで拒否する。拒否した側はすぐ探索へ戻り、招待した側は通知を経て戻る。
        host.update(&[InputAction::Quit], test_config());
        assert!(matches!(host.phase(), LobbyPhase::Discovering));

        for _ in 0..200 {
            client.update(&[], test_config());
            if matches!(client.phase(), LobbyPhase::Notice { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(client.phase(), LobbyPhase::Notice { message, .. } if message == "相手に断られました"),
            "断られたことが通知として出るはず"
        );
    }

    #[test]
    fn an_invite_that_is_never_answered_times_out_into_a_notice() {
        let (mut host, mut client) = facing_lobbies();
        discover_each_other(&mut host, &mut client);
        client.update(&[InputAction::Confirm], test_config());

        // 実時間で10秒待つ代わりに、送信時刻をタイムアウトぶん過去へ倒す。
        let target = match &client.phase {
            LobbyPhase::AwaitingInviteResponse { target, .. } => target.clone(),
            _ => panic!("前提: 応答待ちのはず"),
        };
        client.phase = LobbyPhase::AwaitingInviteResponse {
            target,
            sent_at: Instant::now() - Duration::from_millis(INVITE_TIMEOUT_MS),
        };
        client.update(&[], test_config());

        assert!(
            matches!(client.phase(), LobbyPhase::Notice { message, .. } if message == "応答がありませんでした")
        );
    }

    #[test]
    fn a_notice_returns_to_discovering_once_it_has_been_shown_long_enough() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string()).unwrap();
        lobby.phase = LobbyPhase::Notice {
            message: "テスト".to_string(),
            shown_at: Instant::now() - Duration::from_millis(NOTICE_DISPLAY_MS),
        };

        lobby.update(&[], test_config());

        assert!(matches!(lobby.phase(), LobbyPhase::Discovering));
    }

    #[test]
    fn accepting_an_invite_connects_both_sides_and_starts_a_battle() {
        // 探索→招待→承諾→TCP接続→ハンドシェイクまでを通しで確認する。
        // ハンドシェイクは互いの送受信を待ち合わせるため、2つのロビーを別スレッドで回す。
        let (mut host, mut client) = facing_lobbies();
        discover_each_other(&mut host, &mut client);

        // 招待を送り、相手がそれを受け取るところまで進める。
        client.update(&[InputAction::Confirm], test_config());
        for _ in 0..200 {
            host.update(&[], test_config());
            if matches!(host.phase(), LobbyPhase::IncomingInvite { .. }) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(host.phase(), LobbyPhase::IncomingInvite { .. }));

        // 承諾するとホスト役はlistenerで待ち受け、招待側はACCEPTを受けてconnectする。
        let host_side = thread::spawn(move || run_until_battle(&mut host, &[InputAction::Confirm]));
        let client_opponent = run_until_battle(&mut client, &[]);
        let host_opponent = host_side.join().unwrap();

        assert_eq!(
            host_opponent.as_deref(),
            Some("client"),
            "ホスト役は相手の表示名を受け取って対戦を開始するはず"
        );
        assert_eq!(
            client_opponent.as_deref(),
            Some("host"),
            "クライアント役も同様に対戦を開始するはず"
        );
    }

    /// 対戦が成立するまでロビーを回し、成立したら相手の表示名を返す。
    /// `first_actions`は最初の1回だけ渡す操作(承諾のEnter等)。
    fn run_until_battle(lobby: &mut LobbyState, first_actions: &[InputAction]) -> Option<String> {
        let mut actions = first_actions.to_vec();
        for _ in 0..400 {
            match lobby.update(&actions, test_config()) {
                LobbyOutcome::Battle(state) => return Some(state.opponent_name.clone()),
                LobbyOutcome::Leave => return None,
                LobbyOutcome::Stay => {}
            }
            actions.clear();
            thread::sleep(Duration::from_millis(5));
        }
        None
    }
}
