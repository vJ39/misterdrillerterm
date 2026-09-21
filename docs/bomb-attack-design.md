# 妨害ボムの追加設計(#304)

既存の妨害岩(spec.md 12.8)に、もう1種類の妨害手段としてボムを追加する。岩とボムは別勘定で並行して動く2本のパイプラインとし、既存の岩の仕組みには手を入れない。

## 確定事項(ユーザー合意済み)

1. 岩かボムかの振り分けは、プレイヤーが明示操作で選ぶのではなく、一定比率でランダムに混在させる
2. 岩用・ボム用の攻撃力プール(未送信ぶん・受信待ちぶん)は別勘定にする。相殺も種類ごとに別々に行う
3. 妨害で降ってくるボムは、既存の`Bomb`(白ボン登場→転がり→静止→起爆カウントダウン→誘爆・爆風)をそのまま流用する。専用の演出は作らない

## 変更しない部分

- 攻撃力の計測(直接掘削・4連結以上の自動消滅で消したブロック数を単純合算)は既存のまま
- 既存の岩パイプライン(`attack_power_pending` → 相殺 → `incoming_attack_power` → `try_start_incoming_wave`)の名前・挙動は変えない(実質「rock用」として扱うだけ)

## 送信側: 比率による振り分け

`Game`が消したブロック数を`attack_power_pending`(既存名のまま、岩用として維持)に単純合算するところは変更しない。

相手へ送信するタイミング(`BattleState::exchange_attack_power`が`take_pending_attack_power`を呼ぶ箇所)で、新設定`attack_bomb_ratio_percent`(0〜100%)に従って送信量の一部をボムぶんへ振り分ける。「ランダムに混在」の趣旨に合わせ、四捨五入の固定比率ではなく、`Game`の既存乱数ストリーム(`self.rng`)で1ブロックごとに二項判定し、実際に降る個数にも毎回ばらつきが出るようにする。

```rust
// game/attack.rs に追加。既存のtake_pending_attack_power()は変更しない(岩用として残す)。
pub fn take_pending_attack_power_split(&mut self) -> (u32 /* rock */, u32 /* bomb */) {
    let total = self.take_pending_attack_power();
    let mut bomb = 0u32;
    for _ in 0..total {
        if self.rng.random_range(0.0..1.0) < self.attack_bomb_ratio_percent as f64 / 100.0 {
            bomb += 1;
        }
    }
    (total - bomb, bomb)
}
```

## プロトコル: `GameMessage::Attack`の拡張

```rust
// net.rs
Attack {
    rock_amount: u32,
    bomb_amount: u32,
    proxy_for: Option<usize>,
},
```

1メッセージにrock/bomb両方の量を持たせ、メッセージ数は増やさない。`battle.rs`の送信経路(`exchange_attack_power`。自分ぶん・AI代理送信ぶんの両方)を`take_pending_attack_power_split`に差し替える。ローカル専用対戦(通信なし。#252/#273テスト・#296のAI対戦)の経路も同様に両方の量を渡す。

## 受信側: 別プールでの相殺

`Game`に新規フィールドを追加する(既存の`attack_power_pending`/`incoming_attack_power`は岩用として維持):

```rust
bomb_power_pending: u32,   // 自分の未送信ボム攻撃力
incoming_bomb_power: u32, // 受信待ちのボム攻撃力プール
```

`receive_incoming_attack`のシグネチャを拡張し、rock分・bomb分それぞれ独立に相殺・プール加算する:

```rust
pub fn receive_incoming_attack(&mut self, rock_amount: u32, bomb_amount: u32) -> AttackReceipt
```

`AttackReceipt`もrock/bombそれぞれの`absorbed`/`delivered`/`queued`を持つ形に拡張する(既存フィールド名を`rock_*`に読み替え、`bomb_*`を追加)。既存テストはこの構造変更に合わせて更新する。

## ボムへの変換と投下

新設定値:
- `attack_blocks_per_bomb`(ボム1個に必要な攻撃力。既定値は岩より高め=20を仮置き。岩より重い妨害として扱う)
- `attack_bombs_per_wave_max`(1ウェーブで降らせるボムの上限。既定2)

新規関数`try_start_incoming_bomb_wave`(既存`try_start_incoming_wave`と並行する構造)。岩と違い「予告→出現」の2段階を持たず、既存の自然発生ボムと同じ1段階(判定した瞬間にEnteringフェーズのBombをpush)にする。既存の`spawn_bomb_at_random_empty_cell`(bomb.rs、現在private)を`pub(super)`にして、`attack.rs`から呼べるようにする。既存の上限`BOMB_MAX_COUNT_ON_BOARD`は自然発生ボムと共有し、超えていれば出現を待つ。

## 演出

既存の岩の「控えめな予告」(#247設計)とは対照的に、ボムは既存のBomb演出をそのまま使うため、白ボンの登場・転がり・点滅・起爆カウントダウンSEがそのまま鳴る。プレイヤーは相手の妨害由来か自然発生かを区別できない(区別する演出は作らない)。

## 状態と巻き戻し

`bomb_power_pending`/`incoming_bomb_power`は既存の岩用フィールドと同じ扱いで、フレーム巻き戻し(16章)のスナップショットに含める。新設定値3つ(`attack_blocks_per_bomb`/`attack_bombs_per_wave_max`/`attack_bomb_ratio_percent`)は設定値なので巻き戻し対象外(既存の岩の設定値と同じ扱い)。

## 設定画面への追加項目

| 項目 | 内容 |
|---|---|
| 対戦: ボム1個に必要な攻撃力 | `attack_blocks_per_bomb` |
| 対戦: 一度に降るボムの上限 | `attack_bombs_per_wave_max` |
| 対戦: 攻撃力のボム化比率 | `attack_bomb_ratio_percent`(0〜100%) |

いずれも`settings.json`へ永続化し、対戦では`BattleConfig`でホストの値を一方的に採用する(既存の岩設定と同じ扱い)。

## 変更が及ぶファイル

- `net.rs`: `GameMessage::Attack`拡張、既存テスト(ラウンドトリップ)更新
- `game/attack.rs`: 別プール追加、`take_pending_attack_power_split`、`receive_incoming_attack`拡張、`try_start_incoming_bomb_wave`新規、既存テスト更新
- `game/bomb.rs`: `spawn_bomb_at_random_empty_cell`を`pub(super)`化
- `game/mod.rs`(Game構造体・`update`/`restore_for_rewind`): 新規フィールド追加、巻き戻し対応
- `battle.rs`: `exchange_attack_power`の送受信をrock/bomb両対応に
- `settings.rs`: 新規設定値3つ・クランプ・永続化・`BattleConfig`反映
- `ui/render.rs`: 設定画面に3項目追加
- `docs/spec.md`: 12.8節を拡張(この設計をマージ)
