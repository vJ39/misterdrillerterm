# 4人対戦 段階A: N人対応コア(#273)

対戦人数を2人固定から最大4人(N=2〜4)に拡張する4段階(#273〜#276)の最初の段階。通信は一切繋がず、既存の`BattleState`(2人専用、`game_local`/`game_remote`)をN人分の`Vec<Game>`ベースに再設計し、決着判定を「Win/Lose/Draw」の3値から「順位(1位〜N位)」方式に一般化する。検証はローカル専用(#252相当)のユニットテストのみで行う。

## 1. 通信トポロジの前提(段階B以降のための確認)

フルメッシュ(全員が全員と直接接続)を採用する(ユーザー選定済み)。N人ならC(N,2)本のTCP接続(N=4なら6本)。各プレイヤーは自分以外のN-1人それぞれと直接接続を持つ。この段階Aでは接続そのものは作らないが、「自分が持つ`Vec<Game>`のindex 1..Nが、後で確立するN-1本の接続と1対1で対応する」という前提でデータ構造を作る。

## 2. `lockstep::run_tick_n`(N人版のtick実行)

既存の`lockstep::run_tick`(2人専用)は変更しない(既存テストが依存しているため)。新たにN人版を追加する。

```rust
/// lockstepの1tickぶんの処理をN人向けに一般化したもの(#273)。処理順序は既存の
/// `run_tick`と同じ(全員の入力適用→全員のupdate)で、人数だけが可変になる。
pub fn run_tick_n(games: &mut [Game], actions: &[Option<InputAction>]) {
    debug_assert_eq!(games.len(), actions.len());
    for (game, &action) in games.iter_mut().zip(actions.iter()) {
        if let Some(action) = action {
            game.apply_input(action);
        }
    }
    for game in games.iter_mut() {
        game.update(Duration::from_millis(NET_TICK_MS));
    }
}
```

テスト: 既存の`run_tick`のテスト(`lockstep_keeps_both_hosts_views_in_sync_over_many_ticks`等)と同じ観点を`run_tick_n`でも確認する。加えて、`games.len() == 2`で`run_tick_n`を呼んだ結果が既存の`run_tick`と完全に一致すること(state_hash比較)を確認するテストを1本追加する(N人版が2人版の正しい一般化であることの裏付け)。

## 3. `BattleState`の再設計(`src/battle.rs`)

既存フィールドを削除・変更せず、Vecベースの新しいコンストラクタ・メソッドを追加し、既存の2人専用テストが動くうちは残す方針だと二重管理になるため、**既存の`game_local`/`game_remote`は`games: Vec<Game>`へ統合し、既存の2人専用テストも新しいAPIに合わせて書き直す**(設計書外の勝手な二重実装を避けるため)。

```rust
pub struct BattleState {
    /// 自分を含む全参加者の盤面。index 0が自分。フルメッシュなので、対戦中は全参加者の
    /// 視点をローカルに保持する(2人対戦の`game_local`/`game_remote`のN人への一般化)。
    games: Vec<Game>,
    /// 各参加者の表示名。`games`と同じindexで対応する。
    player_names: Vec<String>,
    net_tick_accum: Duration,
    /// 確定した順位(1が1位)。`games`と同じindexで対応する。全員分`Some`になったら
    /// 全参加者の決着が出たことになるが、`outcome`は自分(`ranks[0]`)が確定した時点で
    /// 決まる(2人版が「自分の状態が確定したら即座にoutcome確定」だったのと同じ)。
    ranks: Vec<Option<u8>>,
    /// 次にゴール到達した参加者へ割り振る順位(1から始まり、確定するたびに人数分進む)。
    next_win_rank: u8,
    /// 次に脱落した参加者へ割り振る順位(Nから始まり、確定するたびに人数分下がる)。
    next_lose_rank: u8,
    outcome: Option<BattleOutcome>,
    /// `Some`なら実際の通信で他参加者の入力を得る(#274)。`None`なら通信なしの
    /// ローカル専用動作(この段階Aのテストが使う経路)。
    peers: Option<Vec<PeerLink>>, // 段階Aでは型だけ用意し、常にNoneのまま使わない
}
```

`PeerLink`は段階B(#274)で定義する。段階Aでは`peers`フィールドを持たせるだけにして、実体は作らない(コンパイルを通すため`peers: Option<Vec<()>>`のような仮置きにはせず、段階Aの時点では`peers`フィールド自体を持たせない方が単純。**設計者の判断でどちらでもよいが、YAGNI的には段階Aでは`peers`フィールドを持たせず、段階Bで追加する方を推奨する**)。

### `BattleOutcome`の再設計

```rust
/// 対戦の決着(#273)。自分視点の最終順位で表す(2人版のWin=1位/Lose=2位/Draw=同着への
/// 一般化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleOutcome {
    /// 自分の最終順位(1が1位)。同着は同じ順位になる。
    Ranked(u8),
    /// StateHashの不一致を検出して対戦を中断した(#255。段階Bで配線、段階Aでは使わない)。
    Desync,
}
```

## 4. 順位判定ロジック

```rust
impl BattleState {
    /// 全参加者の`games`から、まだ確定していない参加者の順位を更新する。
    /// ゴール到達(Cleared)は先着順で上位から、脱落(GameOver)は最後まで残った順で
    /// 上位から埋まる(2人版のWin/Lose/Drawの一般化)。同一tickで複数人が同時に
    /// Cleared/GameOverになった場合は同順位にする。
    fn update_ranks(&mut self) {
        let newly_cleared: Vec<usize> = self.games.iter().enumerate()
            .filter(|&(i, g)| self.ranks[i].is_none() && g.status == GameStatus::Cleared)
            .map(|(i, _)| i).collect();
        let newly_over: Vec<usize> = self.games.iter().enumerate()
            .filter(|&(i, g)| self.ranks[i].is_none() && g.status == GameStatus::GameOver)
            .map(|(i, _)| i).collect();

        if !newly_cleared.is_empty() {
            let rank = self.next_win_rank;
            for &i in &newly_cleared {
                self.ranks[i] = Some(rank);
            }
            self.next_win_rank += newly_cleared.len() as u8;
        }
        if !newly_over.is_empty() {
            let rank = self.next_lose_rank - (newly_over.len() as u8 - 1);
            for &i in &newly_over {
                self.ranks[i] = Some(rank);
            }
            self.next_lose_rank -= newly_over.len() as u8;
        }

        // 未確定者が1人だけ残ったら、その人は自動的に確定する(2人版で「相手が脱落したら
        // 自分は自動的にWin」だったのと同じ。残り1人はゴールも脱落もしていなくても
        // 順位が確定する)。
        let undecided: Vec<usize> = (0..self.games.len())
            .filter(|&i| self.ranks[i].is_none())
            .collect();
        if undecided.len() == 1 {
            self.ranks[undecided[0]] = Some(self.next_win_rank);
        }

        if self.outcome.is_none() {
            if let Some(rank) = self.ranks[0] {
                self.outcome = Some(BattleOutcome::Ranked(rank));
            }
        }
    }
}
```

`next_win_rank`は`1`から、`next_lose_rank`は`games.len() as u8`から初期化する。

### 検証すべき同値性(2人版との整合性)

- 自分がCleared、相手がPlaying → `ranks[0] = Some(1)`(2人版のWin)
- 両者同時Cleared → `ranks[0] = ranks[1] = Some(1)`(2人版のDraw)
- 相手がGameOver、自分がPlaying → `undecided.len()==1`で自分が`Some(1)`確定(2人版のWinの一般化。「相手が脱落したら自動的に自分が勝ち」に対応)
- 自分がGameOver、相手がPlaying → 同様に相手が`Some(1)`、自分は`next_lose_rank`(=2)で確定済み(2人版のLose)
- 両者同時GameOver → `next_lose_rank`(2) - (2-1) = 1 → 両者`Some(1)`(2人版のDraw)

## 5. `advance`(通信なし版、既存ロジックの一般化)

```rust
pub fn advance(&mut self, delta: Duration, local_action: Option<InputAction>) {
    if self.peers.is_some() {
        // 段階Bで実装。段階Aでは到達しない。
        unimplemented!();
    }
    if self.outcome.is_some() {
        return;
    }
    self.net_tick_accum += delta.min(Duration::from_millis(DELTA_CLAMP_MS));
    let net_tick = Duration::from_millis(NET_TICK_MS);
    let mut local_action = local_action;
    while self.net_tick_accum >= net_tick {
        self.net_tick_accum -= net_tick;
        let mut actions = vec![None; self.games.len()];
        actions[0] = local_action.take();
        lockstep::run_tick_n(&mut self.games, &actions);
        self.update_ranks();
        if self.outcome.is_some() {
            break;
        }
    }
}
```

`BattleState::new(games: Vec<Game>, player_names: Vec<String>) -> Self`(通信なし版コンストラクタ)を用意する。`games.len()`は2〜4を想定するが、この段階では長さの検証(パニック等)は入れない(呼び出し元が正しい前提。段階C/Dで実際の人数制約を扱う)。

## 6. テスト方針

既存の2人専用テスト(`reaching_the_goal_first_wins`等)を、新しい`Vec<Game>`ベースのAPIへ書き直す(**削除ではなく移行**。観点は変えない)。加えてN=3・N=4のケースを新規に追加する:

- 3人・4人で、1人がゴール到達→他はPlaying → その1人が`Ranked(1)`
- 3人・4人で、複数人が同一tickでゴール到達 → 同順位
- 4人で、3人が順番に脱落し1人だけ残る → 残った1人が自動的に`Ranked(1)`確定
- 4人で、1人がゴール・1人が脱落・残り2人がPlaying中 → ゴールした人は`Ranked(1)`確定、脱落した人は`Ranked(4)`確定、残り2人は未確定のまま`outcome`が`None`(自分がその2人のどちらでもない前提のテストと、自分がその2人の一方であるテストの両方を用意する)
- `run_tick_n`が`games.len()==2`のとき既存の`run_tick`と同一結果になることの確認(state_hash比較)

## 7. 次段階との境界

- #274(段階B): `PeerLink`を追加し、`peers`が`Some`のときの`advance`(通信あり版)を実装する。既存の2人版`NetworkLink`はこの段階で`PeerLink`へ統合・削除する(二重に残さない)
- #275(段階C): N人のハンドシェイク・フルメッシュ接続確立
- #276(段階D): ロビーUIのN人対応

## 8. 注意点(実装者向け)

- 既存の`resolve_outcome`関数・`BattleOutcome::Win/Lose/Draw`は削除し、上記の`update_ranks`+`BattleOutcome::Ranked`に置き換える。`Desync`バリアントは残す(値は変えない)。
- `game_local`/`game_remote`という名前を持つ既存コード(main.rs、app/screens.rs、ui/render.rsのdraw_battle等)がこの変更で壊れる。**この段階Aではbattle.rs内部の変更に留め、これらの呼び出し元(tick_battle/draw_battle)の追従は必要最小限(コンパイルを通す)にとどめる**。UIの本格的なN人対応(相手情報の複数人表示等)は#270(サイドビュー表示)・#276(ロビーUI)の範囲。
- モジュール可視性は「まず非pubで試す→ビルドエラーが出た箇所だけ最小限pub化する」の順で進める。
