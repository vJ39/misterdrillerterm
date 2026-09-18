# UDP探索とロビーUI(#256)

spec.md 12.1(UDP探索)の実装と、#251〜#255で作った通信基盤(ハンドシェイク・lockstep・デシンク検出)をタイトルから実際に呼び出せる入口を作る。これで#10(ネットワーク対戦モード)がタイトルから実際にプレイできる状態になる。

## 1. UDPパケットのバイナリフォーマット(`src/net.rs`に追加)

spec.md 12.1のテーブル通り、固定長60バイト・リトルエンディアンで手書きエンコード/デコードする(`GameMessage`のbincodeとは別方式。UDPは相手の実装が起動直後で`bincode`の型に依存させたくないため、仕様通りの固定バイナリにする)。

```rust
pub const DISCOVERY_PORT: u16 = 39393;
pub const DEFAULT_TCP_PORT: u16 = 39394;
pub const DISCOVERY_TIMEOUT_MS: u64 = 5000;
pub const INVITE_TIMEOUT_MS: u64 = 10000;
pub const TCP_CONNECT_TIMEOUT_MS: u64 = 3000;
pub const HELLO_BROADCAST_INTERVAL_MS: u64 = 1000;

const DISCOVERY_PACKET_LEN: usize = 60;
const DISCOVERY_MAGIC: [u8; 4] = *b"MDT1";
const PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType { Hello, Invite, Accept, Decline, Bye }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPacket {
    pub packet_type: PacketType,
    pub sender_id: Uuid,
    /// HELLO/BYEでは全ゼロ(`Uuid::nil()`)。
    pub target_id: Uuid,
    pub player_name: String,
    pub tcp_port: u16,
}

impl DiscoveryPacket {
    pub fn encode(&self) -> [u8; DISCOVERY_PACKET_LEN] { ... }
    /// 長さ不足・magic不一致・不明なpacket_typeなら`None`。
    pub fn decode(bytes: &[u8]) -> Option<Self> { ... }
}
```

- `player_name`のUTF-8エンコードはchar境界を壊さないよう切り詰める(spec.md 12.7)。16バイトに収まらない場合は文字単位でtruncateしてから0パディングする。
- `reserved`(4バイト)は送信時全0、受信時は無視する。

## 2. UDP探索の状態(新規 `src/discovery.rs`)

```rust
pub struct DiscoveredPeer {
    pub sender_id: Uuid,
    pub player_name: String,
    pub addr: IpAddr,
    pub tcp_port: u16,
    last_seen: Instant,
}

pub struct Discovery {
    socket: UdpSocket, // non-blocking, SO_BROADCAST有効
    my_id: Uuid,
    my_name: String,
    my_tcp_port: u16,
    last_hello_sent: Instant,
    peers: Vec<DiscoveredPeer>,
}

impl Discovery {
    /// `0.0.0.0:39393`にbindし、非ブロッキング+ブロードキャスト送信可能にする。
    pub fn start(my_name: String, my_tcp_port: u16) -> io::Result<Self> { ... }
    pub fn my_id(&self) -> Uuid { self.my_id }

    /// 1回のtickで行うこと: (a) 前回送信からHELLO_BROADCAST_INTERVAL_MS経っていれば
    /// HELLOを再送、(b) 受信キューを空になるまで処理してpeersを更新、
    /// (c) DISCOVERY_TIMEOUT_MSを超えた候補をpeersから除去。
    /// 戻り値は、この呼び出しで新たに受信したINVITE/ACCEPT/DECLINEのうち、
    /// 自分宛(target_id==my_id)のものだけを返す(HELLOはpeers更新のみで
    /// 呼び出し元には返さない)。
    pub fn tick(&mut self) -> Vec<DiscoveryPacket> { ... }

    pub fn peers(&self) -> &[DiscoveredPeer] { &self.peers }

    pub fn send_invite(&self, target: &DiscoveredPeer) -> io::Result<()> { ... }
    pub fn send_accept(&self, target: &DiscoveredPeer) -> io::Result<()> { ... }
    pub fn send_decline(&self, target: &DiscoveredPeer) -> io::Result<()> { ... }
    /// 探索/募集からの離脱(接続確立時・ロビーを抜ける時に送る)。
    pub fn send_bye(&self) { ... } // 失敗しても対戦の成否に影響しないため戻り値を無視してよい呼び方でもよい
}
```

- ブロードキャスト送信は`255.255.255.255:DISCOVERY_PORT`宛のみでよい(spec.mdのdirected broadcastフォールバックは複数NIC環境向けの拡張であり、この#256では実装しない。単一LAN・単一NIC前提の最小実装にとどめる)。
- 受信は`socket.recv_from`を非ブロッキングで呼び、`WouldBlock`なら空扱いにしてループを抜ける。

## 3. ロビー画面の状態(新規 `src/lobby.rs`)

```rust
pub struct LobbyState {
    discovery: Discovery,
    selection: usize,
    phase: LobbyPhase,
}

enum LobbyPhase {
    Discovering,
    AwaitingInviteResponse { target: DiscoveredPeer, sent_at: Instant },
    IncomingInvite { from: DiscoveredPeer },
    /// ACCEPTを送った直後、自分がTCPサーバ役としてlistenしながら相手のconnectを待つ。
    ConnectingAsHost { listener: TcpListener, opponent_name: String, started: Instant },
    /// ACCEPTを受け取った直後、自分がTCPクライアント役としてconnectを試みる。
    ConnectingAsClient { addr: SocketAddr, opponent_name: String },
    /// 何らかの理由(拒否・タイムアウト・接続失敗)で最後に出す短いメッセージ。
    /// 次のtickでDiscoveringへ自動的に戻る。
    Notice { message: String, shown_at: Instant },
}
```

`LobbyState::new(my_name: String) -> io::Result<Self>`で`Discovery::start`を呼ぶ。`my_tcp_port`は`DEFAULT_TCP_PORT`から使用中なら1つずつ空きを探す(spec.md 12.1「使用中なら39395, 39396…」)。空きポート探索は`TcpListener::bind`を候補ポートで順に試し、成功したポートを`Discovery`のHELLOに載せる。このため、`Discovery::start`より先に(または同時に)listenerを1つ確保しておく必要がある。設計としては、**ロビーに入った時点でTCP listenerを1つ確保しておき(`ConnectingAsHost`はこのlistenerを流用する)**、`LobbyState`に`listener: TcpListener`を持たせて`Discovery`のHELLOにそのポートを載せ続ける形にする。

```rust
pub struct LobbyState {
    discovery: Discovery,
    listener: TcpListener, // 常時listenしておく。ConnectingAsHostへ入る前もHELLOのtcp_portに載せるため必要
    selection: usize,
    phase: LobbyPhase,
}
```

## 4. `Screen`拡張とtick関数(`src/main.rs`・`src/app/screens.rs`)

```rust
enum Screen {
    ...
    NetworkLobby(Box<LobbyState>),
    Battle(Box<BattleState>),
}

enum ScreenTransition {
    ...
    ToNetworkLobby(Box<LobbyState>),
    ToBattle(Box<BattleState>),
}
```

`tick_network_lobby(app: &mut App, state: &mut LobbyState, terminal: &mut ratatui::DefaultTerminal) -> io::Result<Option<ScreenTransition>>`をscreens.rsに新設する。フェーズごとの処理:

- **Discovering**: `state.discovery.tick()`を呼び候補リストを更新。入力は上下キーで`selection`移動、Enterで選択中の候補へ`send_invite`し`AwaitingInviteResponse`へ、Escで`send_bye`してタイトルへ戻る(`ScreenTransition::ToTitle`)。候補が届いた`Invite`パケットがあれば(自分宛)`IncomingInvite`へ遷移する。
- **AwaitingInviteResponse**: `discovery.tick()`の戻り値に`Accept`/`Decline`(自分宛かつ`sender_id == target.sender_id`)があれば処理。`Accept`→`ConnectingAsClient`(相手の`addr`+`tcp_port`から`SocketAddr`を組み立てる)。`Decline`→`Notice`("相手に断られました"等)。`INVITE_TIMEOUT_MS`超過→`Notice`("応答がありませんでした")。Escでキャンセルし`Discovering`へ戻る(BYEは送らない、探索自体は継続するため)。
- **IncomingInvite**: Y(Confirm相当)で承諾: `send_accept`→`ConnectingAsHost`(既存の`listener`をそのまま使う)。N(Quit相当のNキー等、既存の入力体系に合わせる)で拒否: `send_decline`→`Discovering`。
- **ConnectingAsHost**: `listener.accept()`を非ブロッキングで毎tickポーリング(`listener.set_nonblocking(true)`はロビー開始時に設定済み)。接続確立したら`net::run_host_handshake`を呼び(`BattleConfig::from_settings(&app.settings, app.settings.last_course_depth_m)`を渡す)、成功したら`BattleState::from_handshake`→`ScreenTransition::ToBattle`。`TCP_CONNECT_TIMEOUT_MS`を超えても接続が来なければ`Notice`→`Discovering`。
- **ConnectingAsClient**: `TcpStream::connect_timeout(&addr, Duration::from_millis(TCP_CONNECT_TIMEOUT_MS))`を1回呼ぶ(この関数自体がタイムアウト付きでブロックするため、tick内で直接呼んでよい。他のtick処理より若干長くフレームが止まるが、対戦成立までの一度きりの遷移なので許容する)。成功したら`net::run_client_handshake`→`BattleState::from_handshake`→`ScreenTransition::ToBattle`。失敗したら`Notice`→`Discovering`。
- **Notice**: 一定時間(例: `NOTICE_DISPLAY_MS` = 1500ms)経過したら`Discovering`へ自動遷移する。

ハンドシェイク・`BattleState::from_handshake`の呼び出しはブロッキングだが、対戦成立の一度きりの処理であり、既存の`TcpStream`はデフォルトでブロッキングのまま(#253/#254の設計を踏襲)なので、この関数呼び出し中はTUIが一瞬止まる。数百ms〜数秒程度であり許容する。

## 5. タイトルからの入口(`src/input.rs`・`src/app/screens.rs`)

`AnyKeyAction`に`OpenNetworkLobby`を追加し、Nキーに割り当てる。`tick_title`の`match`に以下を追加する:

```rust
input::AnyKeyAction::OpenNetworkLobby => {
    let my_name = format!("Player-{}", &uuid::Uuid::new_v4().to_string()[..4]);
    // 名前入力UIは作らない(#256のスコープ外)。自動生成した表示名をそのまま使う。
    match lobby::LobbyState::new(my_name) {
        Ok(state) => return Ok(Some(ScreenTransition::ToNetworkLobby(Box::new(state)))),
        Err(_) => {} // ソケット確保に失敗(ポート使用中等)。タイトルに留まる。エラー表示はしない。
    }
}
```

## 6. 対戦の決着後の結果表示・退出(`src/battle.rs`・`src/app/screens.rs`)

#252/#254時点の`tick_battle`は、決着(`outcome`が`Some`)後もtickを進めないだけで、画面遷移や結果表示を行っていない。これを#256で追加する。

`BattleState`に`pub fn outcome(&self) -> Option<BattleOutcome>`のgetterを追加する(既存の`outcome`フィールドはprivateのまま)。

`tick_battle`を次のように拡張する:

- 決着前は現状通り。
- `state.outcome()`が`Some`になったら、以後は`state.advance`を呼ぶだけ(#254の実装により、`advance_networked`は決着後も`drain_network_events`・`reconcile_remote_result`を続けるため、Result照合のために呼び続ける必要がある)。加えて描画を結果表示に切り替える(`ui::render::draw_battle`に加えて結果メッセージを重ねる、または新規`ui::render::draw_battle_result`)。
- 結果画面でConfirm(EnterまたはSpace)またはQuitキーが押されたら、`send_bye`相当の処理(`BattleState`が保持する`TcpStream`へ`GameMessage::Bye`を送ってから)`ScreenTransition::ToTitle`で戻る。`BattleState`に`pub fn notify_bye(&mut self)`(通信ありの場合のみ`GameMessage::Bye`を送る、通信なしなら何もしない)を追加する。

結果メッセージの文言(`BattleOutcome`→表示文字列):
- `Win` → "YOU WIN"
- `Lose` → "YOU LOSE"
- `Draw` → "DRAW"
- `Desync` → "DESYNC - DRAW"(spec.md「TUIにはデシンク終了である旨を表示する」)

## 7. 描画(`src/ui/render.rs`)

新規`draw_network_lobby`関数(候補リスト・招待ダイアログ・接続中メッセージ・Notice表示を`state.phase`で分岐)。既存の`draw_title`等と同じ書式(Block+Paragraph)に倣う。`draw_battle`に決着後の結果オーバーレイを追加する(`draw_settings`/`draw_help`と同様、`Clear`+`Paragraph`で中央に重ねる)。

## 8. テスト方針

ネットワーク越しのUI操作は自動テストで検証しづらいため、以下に絞る:

- `DiscoveryPacket::encode`/`decode`の往復テスト(全パケット種別、`player_name`のマルチバイト文字切り詰めを含む)
- `Discovery`のpeer管理: ユニキャスト(127.0.0.1宛の直接`send_to`)でテスト用のHELLOパケットを送り、`peers()`に反映されること、`DISCOVERY_TIMEOUT_MS`超過で除去されることを確認する(ブロードキャストは環境依存でテストが不安定になるため、テストでは宛先を直接指定する)
- `LobbyState`のフェーズ遷移ロジック(`Discovering`→`AwaitingInviteResponse`→`ConnectingAsClient`等)を、実際のUDP/TCPソケットペア(ループバック)を使った結合テストで検証する。`ConnectingAsHost`/`ConnectingAsClient`が実際に`BattleState`まで到達することを1本、通しで確認する
- `BattleOutcome`→表示文字列の対応表のテスト
- 実際の2ターミナル(または2プロセス)での対戦動作確認は自動テストでは行わず、実装後に手元で確認する

## 9. 対戦終了後の設定復元について(要件の再確認)

「対戦するときはホスト設定に合わせ、SE/Music/音量は各端末の設定のまま、対戦終了後は元の設定に戻す」という要件は、#253で`BattleConfig`が`Settings`とは独立した値として扱われ、`Settings`自体を書き換えていないことで既に満たされている(対戦中に使う`Game`は`new_game_from_battle_config`が作った別インスタンスで、`app.settings`は対戦中も変更されない)。#256で新たな対応は不要。

## 10. 次タスクとの境界

これで#10(ネットワーク対戦モード実装)の実装は完了する。残るのは実機での2台(または2プロセス)を使った動作確認のみ。
