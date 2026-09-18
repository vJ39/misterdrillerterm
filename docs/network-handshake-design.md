# TCP層: フレーミング+GameMessage+ハンドシェイク設計(#253)

spec.md 12.2の内容を実装する。12.1(UDP探索・#256)・lockstepの継続ループ(Input/Heartbeat/StateHash/Result交換、#254)は対象外。ゴールは「TCP接続が確立済みの2ホストが、Hello交換→StartConfig→SeedAgree→StartCountdownまでのハンドシェイクを完了し、両者が同一の`BattleConfig`・`seed`・カウントダウン開始時刻を持つ」ところまで。タイトルからの入口はまだ無く(#256)、検証はループバックTCP(`127.0.0.1:0`)を使ったユニットテストで行う。

## 1. 依存クレート追加(ユーザー承認済み)

```
cargo add serde --features derive
cargo add bincode
cargo add uuid --features v4
```

`uuid`は#253時点では未使用(sender_id/target_idはUDP探索#256で使う)だが、spec.md 12.7で明記されているため先に追加しておいてよい。実際に使わない場合はCargo.tomlに追加するだけにとどめ、コードから参照しない(未使用依存の警告は出ない)。

## 2. 新規モジュール `src/net.rs`(非pub mod)

### GameMessage

spec.md 12.2のコード例をそのまま実装する。`#[derive(Serialize, Deserialize)]`を付与する。

```rust
#[derive(Serialize, Deserialize)]
pub enum GameMessage {
    Hello { name: String },
    StartConfig(BattleConfig),
    SeedAgree { seed: u64 },
    StartCountdown { start_at_unix_ms: u64 },
    Input { tick: u32, action: NetAction },
    Heartbeat { tick: u32 },
    StateHash { tick: u32, local_hash: u64, remote_hash: u64 },
    Result { reached_goal: bool, tick: u32, time_ms: u64 },
    Bye,
}
```

`Input`/`Heartbeat`/`StateHash`/`Result`は#253では送受信しない(#254の範囲)が、型としては`GameMessage`に含めておく(spec.md通りの完全な型を先に定義し、#254で使う分だけ後から配線する)。

### NetAction

spec.md 12.2の`InputAction`(プロトコル用)。既存の`game::InputAction`と名前が衝突するため`NetAction`と命名する。

```rust
#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum NetAction {
    None,
    MoveLeft,
    MoveRight,
    FaceUp,
    FaceDown,
    Drill,
}
```

`impl From<NetAction> for Option<crate::game::InputAction>`(Noneは`None`、他はそれぞれ対応するバリアントへ)を用意する。逆方向(`game::InputAction` 5操作 → `NetAction`)の変換はtick_battle側(#252の`classify_battle_input`が返す`InputAction`)で必要になるため、`impl From<crate::game::InputAction> for Option<NetAction>`も用意する(5操作以外は`None`)。

### BattleConfig

spec.md 12.2のフィールド一覧をそのまま実装する。`Settings`(`src/settings.rs`)の対応フィールドとほぼ1対1なので、変換関数を用意する:

```rust
impl BattleConfig {
    /// ホスト側が対戦開始時に自分の設定から作る。`depth_goal_m`はモードセレクトで
    /// 選んだコースの値(呼び出し元から渡す。Settings自体は持たない)。
    pub fn from_settings(settings: &Settings, depth_goal_m: usize) -> Self { ... }
}
```

対応表(spec.md 12.2のフィールド名 → `Settings`のフィールド名。同名のものは列挙を省略):

| BattleConfig | Settings |
|---|---|
| depth_goal_m | (引数で渡す。last_course_depth_mではなく、モードセレクトで選んだ値) |
| field_width | field_width |
| rock/air/star/diamond_spawn_rate_percent | 同名 |
| item_clear_above/unify_colors/starify_screen_rate_percent | 同名 |
| color_count / color_cluster_rate_percent | 同名 |
| bomb_spawn_rate_percent / bomb_fuse_ms | 同名 |
| attack_blocks_per_rock / attack_rocks_per_wave_max | 同名 |
| block_fall_tick_ms / player_fall_tick_ms | 同名 |
| shake_duration_ms / move_cooldown_ms / dodge_recovery_ms / chain_vanish_interval_ms | 同名 |

### フレーミング

spec.md 12.2: 4バイトビッグエンディアン長さプレフィックス(u32、ペイロードのバイト数)+ `bincode`でシリアライズしたペイロード。

```rust
pub fn write_message<W: Write>(writer: &mut W, msg: &GameMessage) -> io::Result<()> { ... }
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<GameMessage> { ... }
```

- 書き込み: `bincode::serialize(msg)` → 長さ(u32::to_be_bytes) → ペイロードの順で`writer`へ書く
- 読み込み: 4バイト読んで長さを得る → その長さぶん読む → `bincode::deserialize`
- デシリアライズ失敗・長さが異常(上限を明らかに超える等の妥当性チェックまでは不要。相手が同じ実装である前提でよい)は`io::Error`にマップして呼び出し元へ返す

## 3. ハンドシェイク(`src/net.rs`)

spec.md 12.2のシーケンス1〜3(StartCountdown送信まで)を実装する。ステップ4(Game生成)はこの関数の外(呼び出し元、`src/battle.rs`)に置く — ネットワーク層はメッセージのやり取りだけに専念し、ゲームロジックの組み立ては`battle.rs`の責務にする。

```rust
/// ハンドシェイクの結果。両ホストがこの値を使って#252のBattleStateを組み立てる。
pub struct HandshakeResult {
    pub opponent_name: String,
    pub config: BattleConfig,
    pub seed: u64,
    pub start_at_unix_ms: u64,
}

/// TCPサーバ役(ACCEPTした側)=ホスト。自分の設定を相手に強制適用させる。
pub fn run_host_handshake(
    stream: &mut TcpStream,
    my_name: &str,
    config: BattleConfig,
) -> io::Result<HandshakeResult> { ... }

/// TCPクライアント役(INVITEした側)。ホストの設定を受け取って従う。
pub fn run_client_handshake(
    stream: &mut TcpStream,
    my_name: &str,
) -> io::Result<HandshakeResult> { ... }
```

- ホスト側の処理順: `Hello`受信 → `Hello`送信 → `StartConfig(config)`送信 → シード生成(`rand::rng()`から64bit) → `SeedAgree { seed }`送信 → `StartCountdown { start_at_unix_ms }`送信(現在unix時刻+3000ms)。戻り値の`config`/`seed`は自分が送った値をそのまま使う
- クライアント側の処理順: `Hello`送信 → `Hello`受信 → `StartConfig`受信 → `SeedAgree`受信 → `StartCountdown`受信。戻り値はすべて受信した値
- 想定外のメッセージ種別を受信した場合(例: `Hello`を待っているのに`Bye`が来た)は`io::Error`(`ErrorKind::InvalidData`)を返す。タイムアウト処理(`INVITE_TIMEOUT_MS`等)は#256の範囲のためここでは実装しない(`TcpStream`の読み書きはブロッキングのままでよい)

## 4. Game生成(`src/battle.rs`に追加)

spec.md 12.2ステップ4を関数化する。既存の`start_new_game`(`src/main.rs`)と同様の3段階処理だが、`Settings`ではなく`BattleConfig`から行う点が異なるため、別関数として`battle.rs`に置く(`main.rs`の`start_new_game`は変更しない)。

```rust
/// `BattleConfig`とseedから、通常プレイの開始処理と同一順序でGameを1つ生成する
/// (spec.md 12.2ステップ4)。ホスト・クライアントの双方がこの関数を同じ引数で
/// 呼ぶことで、4インスタンス(各ホスト2つ)の初期盤面が一致する。
pub fn new_game_from_battle_config(seed: u64, config: &BattleConfig) -> Game { ... }
```

中身は`Game::new_with_width(seed, config.field_width, config.depth_goal_m)` → 速度系setter群 → `reroll_spawn_rates_from(2, ...)`(#251/#252時点の`start_new_game`と同じ並び)。

## 5. テスト方針

- フレーミング往復テスト: `Vec<u8>`をバッファに使い、`write_message`→`read_message`で同じ`GameMessage`が復元されることを確認(`GameMessage`に`PartialEq`/`Debug`をテスト用に導出するか、パターンマッチで中身を比較する)
- ハンドシェイク結合テスト: `TcpListener::bind("127.0.0.1:0")`でOSに空きポートを割り当てさせ、`local_addr()`でポート番号を得る。ホスト役をスレッドで`run_host_handshake`実行、メインスレッドで`TcpStream::connect`して`run_client_handshake`実行。両者の`HandshakeResult`の`config`/`seed`/`start_at_unix_ms`が一致することを確認する
- `NetAction`⟷`InputAction`の変換テスト(5操作の往復、対象外操作は`None`になること)
- `new_game_from_battle_config`が同一引数で呼ぶと同一の`state_hash()`を持つGameを2つ生成できることを確認する(#250の`Game::state_hash`を使う)

## 6. 次タスクとの境界

- #254: `run_host_handshake`/`run_client_handshake`の結果を使って`battle::BattleState`を組み立て、通信スレッド+mpscでlockstepループ中の`Input`/`Heartbeat`/`StateHash`/`Result`の継続送受信を実装する。`Screen::Battle`への実際の遷移もここで行う
- #255: `GameMessage::StateHash`の定期送受信をlockstepループへ配線する
- #256: UDP探索・招待ダイアログを実装し、`run_host_handshake`/`run_client_handshake`を呼ぶ入口(ロビーUI)を作る
