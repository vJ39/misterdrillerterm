# デシンク検出(StateHash)の配線(#255)

spec.md 12.3「デシンク検出(StateHash)」を実装する。#254で確立したlockstepのtick進行に、定期的な状態ダイジェスト照合を追加し、実装バグによる非同期(デシンク)を早期に検出して対戦を引き分けで打ち切れるようにする。UDP探索・ロビーUI(#256)は対象外。検証はループバックTCPでの結合テストで行う。

## 1. 新規定数(`src/constants.rs`、`HEARTBEAT_INTERVAL_MS`の近くに追加)

```rust
/// `StateHash`を送る間隔(tick数、spec.md 12.3)。約3秒ごと。
pub const STATE_HASH_INTERVAL_TICKS: u32 = 20;
```

## 2. `BattleState`/`NetworkLink`の拡張(`src/battle.rs`)

デシンクは「勝敗判定(`resolve_outcome`)」とは別系統の理由で対戦を終わらせるため、`BattleOutcome`に新しいバリアントを追加する。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleOutcome {
    Win,
    Lose,
    Draw,
    /// StateHashの不一致を検出して対戦を中断した(spec.md 12.3)。勝敗としては
    /// 引き分けと同様に扱うが、原因が異なるためUI表示を分けられるよう区別する。
    Desync,
}
```

既存の`resolve_outcome`・Result不一致の`Draw`上書きはそのまま`Draw`を使う(変更しない)。デシンク検出だけがこの新バリアントを使う。

`NetworkLink`に以下を追加する:

```rust
struct NetworkLink {
    // ...(既存フィールドはそのまま)
    /// 自分が計算し、まだ相手からの対応する`StateHash`と照合できていない
    /// (local_hash, remote_hash)。tick番号をキーに持つ。相手の到着が自分より
    /// 早い場合と遅い場合の両方があるため、双方向にキューを持つ。
    own_state_hashes: HashMap<u32, (u64, u64)>,
    /// 相手から届いたが、自分がまだそのtickに到達していない`StateHash`。
    pending_remote_state_hashes: HashMap<u32, (u64, u64)>,
}
```

(`use std::collections::HashMap;`を追加。エントリ数は`STATE_HASH_INTERVAL_TICKS`ごとに1件しか増えず、照合が済んだら削除するため無制限に増え続けることはない。)

## 3. 送信

`advance_networked`のtickループ内、`link.next_tick += 1`した直後(#254で`run_net_tick_with_remote`を呼んだ後)に追加する:

```rust
let completed_tick = link.next_tick - 1; // このtickの処理が完了した
if completed_tick % STATE_HASH_INTERVAL_TICKS == 0 {
    let local_hash = self.game_local.state_hash();
    let remote_hash = self.game_remote.state_hash();
    link.own_state_hashes.insert(completed_tick, (local_hash, remote_hash));
    let _ = net::write_message(&mut link.writer, &GameMessage::StateHash {
        tick: completed_tick,
        local_hash,
        remote_hash,
    });
    reconcile_state_hash(link, &mut self.outcome, completed_tick);
}
```

tick 0(`completed_tick == 0`)も送信対象になる(`0 % N == 0`)。両者とも初期盤面から始まるため、ここでの照合は「ハンドシェイクで合意したseed/configから同一の初期盤面が作られているか」の検証にもなる。

## 4. 受信・照合

`drain_network_events`で`GameMessage::StateHash`を受信した処理を追加する:

```rust
NetworkEvent::Message(GameMessage::StateHash { tick, local_hash, remote_hash }) => {
    link.last_remote_activity = Instant::now();
    link.pending_remote_state_hashes.insert(tick, (local_hash, remote_hash));
}
```

照合関数(送信時・受信時の両方から呼ぶ):

```rust
/// 指定tickについて、自分の計算値と相手からの申告値が両方揃っていれば照合する。
/// 相手の`local_hash`(相手自身の盤面)は自分の`game_remote`のそのtick時点の値と、
/// 相手の`remote_hash`(相手から見た自分)は自分の`game_local`のそのtick時点の値と
/// 一致するはず。不一致ならデシンクとして`outcome`を`Desync`にする。
fn reconcile_state_hash(link: &mut NetworkLink, outcome: &mut Option<BattleOutcome>, tick: u32) {
    let Some(&(mine_local, mine_remote)) = link.own_state_hashes.get(&tick) else { return };
    let Some(&(their_local, their_remote)) = link.pending_remote_state_hashes.get(&tick) else { return };

    link.own_state_hashes.remove(&tick);
    link.pending_remote_state_hashes.remove(&tick);

    if outcome.is_some() {
        return; // 既に他の理由で決着済みなら上書きしない
    }
    if mine_remote != their_local || mine_local != their_remote {
        *outcome = Some(BattleOutcome::Desync);
    }
}
```

`drain_network_events`の`StateHash`受信アームでも、挿入直後にこの照合を呼ぶ(自分が先に計算済みで相手の到着を待っていたケースに対応するため)。

## 5. 決着後の扱い

`advance_networked`は決着後(`outcome.is_some()`)も`drain_network_events`だけは呼び続ける(#254で実装済み)。デシンクは対戦終了前の同期ズレを検出するものなので、既に`Win`/`Lose`/`Draw`が確定した後にStateHash不一致を検出しても上書きしない(`reconcile_state_hash`内の`if outcome.is_some() { return; }`で担保する)。

## 6. テスト方針

- 正常系: `connected_pair`で複数tick(`STATE_HASH_INTERVAL_TICKS`を跨ぐ回数)を進め、`link.own_state_hashes`/`pending_remote_state_hashes`が空になっている(=すべて照合済み)ことと、`outcome`が`Desync`になっていないことを確認する。
- 異常系: `battle_with_raw_peer`で、生ストリームから意図的に食い違う`GameMessage::StateHash`(例えば`local_hash`をわざと違う値にする)を送りつけ、`outcome`が`Some(BattleOutcome::Desync)`になることを確認する。
- 決着後にStateHash不一致が届いても、既存の`outcome`(Win/Lose/Draw)が上書きされないことを確認するテストを追加する。

## 7. 次タスクとの境界

- #256: UDP探索・招待ダイアログ・`Screen::Battle`への実際の入口。加えて、`BattleOutcome::Desync`をUIでどう表示するか(spec.md「TUIにはデシンク終了である旨を表示する」)は、`Screen::Battle`の結果表示画面を作る際に対応する(#255では`ui::render::draw_battle`への表示追加は行わない。まだ結果表示画面自体が無いため)。
