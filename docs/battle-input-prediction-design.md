# 対戦入力のクライアントサイド予測描画(#292)

lockstep方式の対戦(`NET_TICK_MS`固定tick+相手の入力待ち)では、自分の入力もtick確定まで盤面(`Game`)に反映されない。#287(TCP_NODELAY)・#288(tick未発火フレームでの入力取りこぼし修正)で対処した後も、「tick確定を待たないと動かない」という構造自体は残っている。これをtick確定を待たず、ローカルで先行して見た目に反映する。

## 方針

`BattleState`に見た目専用の複製`predicted: Game`を追加する。

- 自分の入力(`InputAction`)を受け取った瞬間、確定tickを待たず`predicted`にも同じ入力を`apply_input`で適用し、即座に見た目へ反映する。
- tickが確定するたび(`run_net_tick_with_actions`を呼んだ直後)、`predicted`を`games[0]`(確定後の正式な状態)のクローンで置き換える。これにより、予測がどれだけズレても最大1tick(`NET_TICK_MS`)で必ず正式な状態に同期し直される。
- 描画(`draw_battle`)には`games[0]`の代わりに`predicted`を渡す。相手側(`games[1..]`)の表示は変えない(相手は元々予測しない)。

## スコープを絞る点

- 時間経過(自然落下・アイテム補充等、`Game::update`)は`predicted`側では先行させない。今回は「入力操作(移動・掘削)が即座に反映される」ことだけを対象にする。時間経過も先行させると、tick確定時の巻き戻り(スナップ)が目立ちやすくなるため、まずは入力だけに絞り、必要なら次のステップで検討する。
- 予測は`peers.is_some()`(実際の通信あり)の経路のみに実装する。`peers.is_none()`のローカル専用経路はテストのみで使われ、tickが即時に進むため予測が不要。
- `predicted`側で発生する`GameEvent`(SE再生のトリガー)は無視する。SEは確定後の`games[0]`側のイベントでのみ再生する(対戦画面は現状SEを再生していないため、実質的な影響は無い)。
- `Game`は既に`#[derive(Clone)]`済み(#233)で、対戦用の`Game`は`debug_log`が常に`None`(`refresh_debug_log`を呼ばない)なので、`predicted`側の操作がデバッグログを汚染する心配はない。

## 実装ポイント

- `BattleState::from_peer_streams`/`new`で`predicted: games[0].clone()`として初期化する。
- `advance_networked`の先頭、`local_action`を`pending_local_action`にセットする箇所で、同じ入力を`self.predicted.apply_input(action)`する。
- `run_net_tick_with_actions`を呼んだ直後に`self.predicted = self.games[0].clone();`する。
- 描画側(`app::screens::tick_battle`)は`&state.games[0]`を`state.predicted_game()`(新設アクセサ)に変える。

## 懸念(実測せずに進める点)

`Board.rows`(コース長分の行数×フィールド幅)のクローンコストがtick確定ごと(最短`NET_TICK_MS`=50ms間隔)に発生する。深いコース(1000m)ほど大きくなるが、実測せずに進め、パフォーマンス上問題が出たら別対応(差分更新等)を検討する。
