# 対戦用の画面・状態設計(#252)

spec.md 12章のうち、通信を伴わない「画面・状態」の部分だけを対象にする。UDP探索/ロビーUI(#256)、TCP層(#253)、通信スレッドとの実際のInput交換(#254)、StateHash配線(#255)は対象外。この段階では対戦画面は**タイトルから到達できない**(入口は#256で作る)。検証はユニットテストのみで行う。

## 1. Screen拡張

```rust
enum Screen {
    Title,
    ModeSelect,
    Settings,
    Help,
    Playing(Box<Game>),
    Battle(Box<BattleState>), // 新規
}
```

## 2. BattleState構造体(新規 `src/battle.rs`)

```rust
struct BattleState {
    game_local: Game,
    game_remote: Game,
    opponent_name: String,
    /// 実測フレーム時間をNET_TICK_MS(150ms)単位へ量子化するための蓄積バッファ。
    /// #251のrun_tickは1回で150ms固定分しか進めないため、フレーム間隔が150msの
    /// 倍数からずれてもtickを取りこぼさないよう繰り越す。
    net_tick_accum: Duration,
    outcome: Option<BattleOutcome>,
}

enum BattleOutcome {
    Win,
    Lose,
    Draw,
}
```

- `game_local`/`game_remote`は#251の`lockstep::run_tick`にそのまま渡せる形にする。
- `game_remote`への入力は、この段階ではまだ通信スレッドが無いため**常に`None`**(#254で実際のmpsc受信に差し替える)。`tick_battle`関数内で「相手の入力を取得する」処理を1箇所にまとめておき、#254での差し替えを1箇所の変更で済むようにする。

## 3. 150ms固定tickの生成

```rust
app.net_tick_accum... // BattleState側で保持
let now = Instant::now();
let delta = now.duration_since(app.last_tick);
app.last_tick = now;
state.net_tick_accum += delta.min(Duration::from_millis(250));
while state.net_tick_accum >= Duration::from_millis(NET_TICK_MS) {
    state.net_tick_accum -= Duration::from_millis(NET_TICK_MS);
    let local_action = /* このtickで確定した自分の入力(無ければNone) */;
    let remote_action = None; // #254で差し替え
    lockstep::run_tick(&mut state.game_local, &mut state.game_remote, local_action, remote_action);
}
```

- 1tickにつき自分の入力は高々1アクション(spec.md 12.2)。フレーム内で複数キーが来た場合は最初の1つだけ採用し、残りは次tickへ持ち越さず捨てる(通常プレイの`poll_input_batch`のような全キュー処理はしない)。

## 4. tick_battle関数(新規、`src/app/screens.rs`)

既存の`tick_playing`とは独立した関数にする(理由: `tick_playing`は570行あり、対戦用の分岐を混ぜるとさらに肥大化し、通常プレイ側の挙動を壊すリスクも生む)。

扱う入力は次のみ。**それ以外は全て無視する**(12.5の無効化はこの取捗選択によって実現する。個別の無効化フラグは持たない):

| InputAction | 対戦中の扱い |
|---|---|
| `MoveLeft`/`MoveRight`/`FaceUp`/`FaceDown`/`Drill` | このtickの`local_action`として採用(1tick高々1個) |
| `ToggleMusic`/`ToggleSe` | 通常プレイのPaused限定と異なり、対戦中は**常時**受け付ける(12.5「音声はローカル専用でシミュレーションに影響しない」)。`app.settings`へ即時反映し保存する |
| `Quit` | 対戦を中断してタイトルへ戻る(`ScreenTransition::ToTitleDiscardingGame`と同様の後始末。#254で「切断通知を送ってから戻る」処理を追加する前提のフックとして、関数を分けておく) |
| それ以外(TogglePause/Rewind/OpenSettings/OpenHelp/Debug*/Confirm/UnboundKey等) | 無視。matchの`_ => {}`で握りつぶす |

`GameOver`相当の状態になった場合、通常プレイの「その場から復活」ダイアログは出さない(12.4: 脱落=即敗北)。`game_local.status == GameOver`になった時点で`outcome = Some(Lose)`、`game_remote.status == GameOver`になった時点で`outcome = Some(Win)`(同一tickなら`Draw`)。ゴール到達(`status == Cleared`)も同様に判定する。

`outcome`が`Some`になったら以後は入力を受け付けず、結果表示に専念する(結果表示自体の画面遷移は#254の対戦終了シーケンスで確定させるため、#252では`BattleState`に保持するだけで、まだ`Screen`遷移までは実装しない)。

## 5. 相手盤面の簡易表示

spec.md 12.3「相手盤面の表示は縮小表示や深度・ライフ・進捗バーのみの簡易表示でもよいが、シミュレーション自体は省略してはならない」に従い、フル盤面は描画せず次の3値のみサイドパネルに表示する:

- 深度(m): `game_remote.player.depth_m()`
- ライフ: `game_remote.player.lives`
- 進捗バー: `depth_m / depth_goal_m`

`depth_goal_m`は現状private(`src/game/mod.rs:422`)なので、`src/game/view.rs`に`pub fn depth_goal_m(&self) -> usize`を追加する(既存のgetter群と同じ置き場所)。

新規描画関数`ui::render::draw_battle`(仮)を追加し、自分の盤面は既存の`draw`をレイアウト調整して呼び、右側または下部に上記3値のサイドパネルを重ねる。

## 6. この段階でのテスト方針

タイトルからの入口が無いため、`src/battle.rs`内の`#[cfg(test)]`で`BattleState`を直接構築し、`tick`相当のロジック(入力→outcome確定までの状態遷移)を直接呼び出して検証する。具体的には:

- 5操作をそのまま`run_tick`へ渡すヘルパー関数を`BattleState`のメソッドとして切り出し、UIを介さずテストできるようにする(`tick_playing`のようにterminal描画と混ぜない)
- `game_local`が先にゴール到達 → `outcome == Some(Win)`
- `game_remote`が先に脱落 → `outcome == Some(Win)`(相手の脱落なので自分の勝ち)
- 同一tickで両者ゴール到達 → `outcome == Some(Draw)`
- `ToggleMusic`/`Quit`等の対戦中でも許可される入力と、`TogglePause`/デバッグ系等の無視される入力がそれぞれ意図通りに扱われることを確認するテスト

## 7. 次タスクとの境界

- #253(TCP層): `GameMessage`のシリアライズ/デシリアライズとハンドシェイクのみ。`BattleState`には触れない
- #254(lockstep本番化): 通信スレッドの受信キューを`BattleState.game_remote`への入力ソースとして接続し、`Screen::Battle`への遷移(ハンドシェイク完了時)と対戦終了後の`Screen::Title`遷移(設定を元に戻す処理を含む)を実装する
- #255: `StateHash`をtick_battleループへ配線し、不一致時に`outcome`をデシンク扱いにする分岐を追加
- #256: UDP探索・招待ダイアログ・`Screen::Battle`への実際の入口
