# lockstep本番化(#254)

#251(ローカルlockstepハーネス)・#252(BattleStateと画面)・#253(ハンドシェイク)を実際のTCP通信で繋ぐ。ゴールは「ハンドシェイク完了後の2ホストが、通信スレッド越しにInputを交換しながらlockstepを進行し、タイムアウト・切断・決着(Result交換込み)まで正しく処理できる」こと。デシンク検出(StateHash送受信、#255)とUDP探索・ロビーUI・`Screen::Battle`への実際の入口(#256)は対象外。検証はループバックTCPを使った結合テストで行う。

## 1. 新規定数(`src/constants.rs`、`NET_TICK_MS`の近くに追加)

```rust
/// 次のtickに必要な相手の入力を待つ上限(ms)。超えたら相手の切断とみなし対戦を
/// 中断する(不戦敗にする側は検知した側の相手が勝つ。spec.md 12.3)。
pub const LOCKSTEP_WAIT_TIMEOUT_MS: u64 = 500;
/// InputまたはHeartbeatのいずれも届かない状態がこの時間続いたら切断とみなす
/// (spec.md 12.4)。`LOCKSTEP_WAIT_TIMEOUT_MS`より大きい値にする
/// (tick待ちタイムアウトの方が先に発火するのが通常経路のため)。
pub const HEARTBEAT_TIMEOUT_MS: u64 = 2000;
/// Heartbeatを送る間隔(ms)。Inputは自分のtickが進むたびに送られるため、
/// Heartbeatは「自分のtickも相手のtickも動いていない」状況で生存を示すための
/// 補助メッセージとして、この間隔で定期的に送る。
pub const HEARTBEAT_INTERVAL_MS: u64 = 1000;
```

## 2. 通信スレッド(`src/net.rs`に追加)

```rust
/// 通信スレッドがメインループへ届けるイベント。
pub enum NetworkEvent {
    Message(GameMessage),
    /// 受信ループがエラー(相手の切断・デコード失敗等)で終了した。
    Disconnected,
}

/// 受信専用スレッドを立て、`stream`から届いたメッセージを`tx`へ流し続ける
/// (spec.md 12.7「専用スレッド+mpscでメインのゲームループをブロックしない」)。
/// `GameMessage::Bye`を受信した場合、そのメッセージ自体は`tx`へ送ってからスレッドを
/// 終了する(呼び出し側がBye受信を扱えるように)。読み込みエラーの場合は
/// `NetworkEvent::Disconnected`を送って終了する。
pub fn spawn_receiver_thread(
    mut stream: TcpStream,
    tx: mpsc::Sender<NetworkEvent>,
) -> thread::JoinHandle<()> { ... }
```

送信は専用スレッド化しない。`BattleState`が保持する書き込み用`TcpStream`(`stream.try_clone()`で複製したもの)へ、メインループから直接`write_message`を呼ぶ。TCPの送信バッファはよほどの詰まりがなければ即座に返るため、既存の`ratatui`描画ループを大きくブロックしない前提で単純化する。

## 3. `BattleState`の拡張(`src/battle.rs`)

既存の`BattleState::new`(#252で実装済み、通信なし・相手入力は常に`None`)とそのテストは変更しない。通信の有無を`Option`で切り分ける。

```rust
pub struct BattleState {
    game_local: Game,
    game_remote: Game,
    opponent_name: String,
    net_tick_accum: Duration,
    outcome: Option<BattleOutcome>,
    /// `Some`なら実際の通信で相手の入力を得る(#254)。`None`なら#252までと同じ、
    /// 相手の入力は常に`None`として扱うローカル専用の動作(既存テストの前提)。
    network: Option<NetworkLink>,
}

struct NetworkLink {
    writer: TcpStream,
    event_rx: mpsc::Receiver<net::NetworkEvent>,
    /// Dropさせない目的だけで保持する(スレッド自体は`writer`と無関係に動く)。
    _receiver_thread: thread::JoinHandle<()>,
    /// 次に処理するtick番号。`Input`メッセージの`tick`と突き合わせるのに使う。
    next_tick: u32,
    /// 通信の遅延で自分のtickより先に届いた相手の`Input`を、tick番号付きで
    /// 一時保持する。
    pending_remote_inputs: VecDeque<(u32, NetAction)>,
    /// 次のtickの相手の入力を待ち始めた時刻。`None`なら待機していない
    /// (前のtickまでは順調に揃っていた)。
    awaiting_since: Option<Instant>,
    /// InputまたはHeartbeatを最後に受信した時刻(`HEARTBEAT_TIMEOUT_MS`の判定に使う)。
    last_remote_activity: Instant,
    /// 最後にHeartbeatを送信した時刻。
    last_heartbeat_sent: Instant,
    /// 対戦開始時刻(ハンドシェイクの`StartCountdown`の値)。Result送信時の
    /// `time_ms`(経過時間)の算出に使う。
    start_at_unix_ms: u64,
    /// 自分のResultは送信済みか。決着直後に一度だけ送るためのフラグ。
    result_sent: bool,
    /// 相手のResultを受信済みか。受信後は`resolve_final_outcome`で照合する。
    remote_result: Option<(bool, u32)>, // (reached_goal, tick)
    disconnected: bool,
}
```

### コンストラクタ

```rust
impl BattleState {
    /// #253のハンドシェイク結果と確立済みのTCPストリームから、通信ありの
    /// 対戦状態を組み立てる。`stream`は呼び出し元がハンドシェイクに使ったものを
    /// そのまま渡す(内部で`try_clone`して読み書き用に分ける)。
    pub fn from_handshake(
        handshake: net::HandshakeResult,
        stream: TcpStream,
    ) -> io::Result<Self> {
        let seed = handshake.seed;
        let config = &handshake.config;
        let game_local = new_game_from_battle_config(seed, config);
        let game_remote = new_game_from_battle_config(seed, config);

        let reader_stream = stream.try_clone()?;
        let (tx, event_rx) = mpsc::channel();
        let receiver_thread = net::spawn_receiver_thread(reader_stream, tx);

        Ok(Self {
            game_local,
            game_remote,
            opponent_name: handshake.opponent_name,
            net_tick_accum: Duration::ZERO,
            outcome: None,
            network: Some(NetworkLink {
                writer: stream,
                event_rx,
                _receiver_thread: receiver_thread,
                next_tick: 0,
                pending_remote_inputs: VecDeque::new(),
                awaiting_since: None,
                last_remote_activity: Instant::now(),
                last_heartbeat_sent: Instant::now(),
                start_at_unix_ms: handshake.start_at_unix_ms,
                result_sent: false,
                remote_result: None,
                disconnected: false,
            }),
        })
    }
}
```

`Screen::Battle`への実際の遷移(`from_handshake`をどこから呼ぶか)は#256(ロビーUI)の範囲のため、この段階ではテストからのみ呼ばれる。

## 4. `advance`の同期ロジック変更

`network`が`None`のとき(#252までの既存テスト)は今まで通りの時間ベースの動作を維持する。`Some`のときは以下の手順に変える:

```
advance(delta, local_action):
    if outcome.is_some(): return

    if let Some(link) = &mut network:
        drain_network_events(link)  // Bye/Disconnected/Heartbeat/Input/Resultを処理
        if link.disconnected:
            outcome = Some(Win)  // 相手切断=不戦勝(12.4)
            return
        if let Some(since) = link.awaiting_since:
            if since.elapsed() >= LOCKSTEP_WAIT_TIMEOUT_MS:
                outcome = Some(Win)
                link.disconnected = true
                return
            return  // まだ待機中。時間を蓄積しない(相手を待つ間は自分の時計を進めない)
        if since_last_heartbeat_sent >= HEARTBEAT_INTERVAL_MS:
            send Heartbeat { tick: link.next_tick }
            link.last_heartbeat_sent = now

    net_tick_accum += delta.min(DELTA_CLAMP_MS)
    let mut local_action = local_action
    while net_tick_accum >= NET_TICK_MS:
        if let Some(link) = &mut network:
            let remote = take_remote_input_for(link, link.next_tick)
            match remote:
                None =>
                    link.awaiting_since = Some(Instant::now())
                    break  // このtick分のnet_tick_accum減算はまだ行わない
                Some(net_action) =>
                    net_tick_accum -= NET_TICK_MS
                    let my_action = local_action.take()
                    send Input { tick: link.next_tick, action: my_action.into() }  // NoneはNetAction::None
                    run_net_tick(net_action.into())
                    link.next_tick += 1
                    if outcome.is_some():
                        maybe_send_result(link)
                        break
        else:
            net_tick_accum -= NET_TICK_MS
            run_net_tick(local_action.take())
            if outcome.is_some(): break
```

- `take_remote_input_for(link, tick)`: `pending_remote_inputs`から該当tickのエントリを取り出す(無ければ`drain_network_events`を追加で1回呼んでから再確認してもよいが、`advance`の先頭で既に呼んでいるため二重に呼ぶ必要はない)。
- `drain_network_events(link)`: `event_rx.try_recv()`をエラー(空)になるまでループし、`GameMessage::Input`は`pending_remote_inputs`へpush、`GameMessage::Heartbeat`と`GameMessage::Input`はいずれも`link.last_remote_activity = Instant::now()`を更新、`GameMessage::Result`は`link.remote_result`へ格納、`GameMessage::Bye`と`NetworkEvent::Disconnected`は`link.disconnected = true`。それ以外(`Hello`/`StartConfig`/`SeedAgree`/`StartCountdown`)は本来この段階で届くはずがないため無視してよい(#253のハンドシェイクで既に消費済み)。
  - 併せて`HEARTBEAT_TIMEOUT_MS`を`last_remote_activity`からの経過で判定し、超えていたら`link.disconnected = true`にする。

## 5. Result送信・照合

決着(`outcome`が`Some`になった)直後、`maybe_send_result`で1回だけ`GameMessage::Result`を送る:

```rust
fn maybe_send_result(&mut self) {
    let Some(link) = &mut self.network else { return };
    if link.result_sent { return; }
    link.result_sent = true;
    let reached_goal = self.game_local.status == GameStatus::Cleared;
    let time_ms = unix_time_ms().saturating_sub(link.start_at_unix_ms);
    let _ = net::write_message(&mut link.writer, &GameMessage::Result {
        reached_goal,
        tick: link.next_tick,
        time_ms,
    });
}
```

相手のResultを受信済み(`remote_result`が`Some`)なら、自分視点の`resolve_outcome`結果と突き合わせる: 相手の`reached_goal`が自分の`game_remote.status == Cleared`の判定と食い違う場合(例えば自分は相手が脱落したと判定したのに、相手は自分がゴールしたと主張している)は、12.4「食い違った場合はデシンクと同様に引き分け扱い」に従い`outcome`を`Draw`へ上書きする。この照合は決着後、`advance`が呼ばれるたびに(`drain_network_events`の中で`Result`を受け取った時点で)行ってよい。

## 6. 勝敗判定の全体像(既存`resolve_outcome`との関係)

`resolve_outcome`(#252で実装済み)はそのまま使う。#254で新たに`outcome`が確定する経路は次の3つ:

1. 通常の決着(両者のGameStatusから`resolve_outcome`が`Some`を返す) — 既存のまま
2. 相手切断(タイムアウトまたはBye受信) — `Some(Win)`(自分の不戦勝)
3. Result不一致 — 通常決着後に`Some(Draw)`へ上書き

## 7. テスト方針

- モジュール内`#[cfg(test)]`で、ループバックTCP(`127.0.0.1:0`)を使い、#253のハンドシェイクをそのまま実行してから`BattleState::from_handshake`で両側の`BattleState`を作り、以下を検証する:
  - 双方が異なる入力列を送り合っても、`lockstep::run_tick`と同じ結果(状態ハッシュ一致)でtickが進むこと(#251のテストと同じ発想をTCP経由で確認)
  - 片方が`advance`を呼び続けるのを止める(入力を送らなくなる)と、もう片方が`LOCKSTEP_WAIT_TIMEOUT_MS`後に不戦勝になること
  - 片方が明示的に`GameMessage::Bye`を送ってスレッドを終了すると、もう片方が切断を検知して不戦勝になること
  - 決着後に両者から一貫した`Result`が送られ、そのまま`outcome`が変わらないこと
  - (可能なら)片方の`Result`を意図的に細工して不一致にし、`outcome`が`Draw`へ上書きされることを確認する(テストダブルとして生のTCPストリームに直接`write_message`で偽の`Result`を送るヘルパーを使う)
- `network: None`の既存テスト(#252)は変更しないこと

## 8. 次タスクとの境界

- #255: `drain_network_events`に`GameMessage::StateHash`の処理を追加し、`STATE_HASH_INTERVAL_TICKS`(20 tick)ごとに送信、不一致時に`outcome`をデシンク扱い(引き分け)にする
- #256: UDP探索・招待ダイアログを実装し、`run_host_handshake`/`run_client_handshake` → `BattleState::from_handshake` → `Screen::Battle`遷移という一連の流れを実際にタイトルから呼び出せるようにする
