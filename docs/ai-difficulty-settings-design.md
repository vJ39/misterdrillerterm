# AI専用設定画面の追加設計(#312)

対戦でAIに勝てないという相談から発展。AIの盤面生成に使うパラメータ(落下速度・出現率等)を人間用とは別に持たせ、人間用の設定画面とは完全に分離した専用画面で編集できるようにする。

## 確定事項(ユーザー合意済み)

1. プリセット(Easy/Normal/Hard)は無し。既存の設定画面と同じ「数値を直接増減する」UIをそのまま使う
2. 人間用の設定画面とは完全に別の画面にする(項目数が増えても1画面に詰め込まない)
3. 入り口は2箇所: ロビー画面(I/Dキーでルームへ混ぜるAI人数を決める操作の近く)と、AI対戦(Vキー、通信なし)の人数選択画面の両方
4. 反応速度の差は対策しない(ドッジ回復・移動クールダウンは対象外)。落下速度自体を落とすことで速度差の要素を作る方針

## 対象パラメータ(20項目。既存`Settings`のフィールドと1:1でミラーする)

| 既存フィールド | 意味 |
|---|---|
| `block_fall_tick_ms` | ブロック落下間隔 |
| `player_fall_tick_ms` | キャラ落下間隔 |
| `shake_duration_ms` | 落下前の揺れ時間 |
| `rock_spawn_rate_percent` | 岩ブロック出現率 |
| `air_spawn_rate_percent` | AIR出現率 |
| `star_spawn_rate_percent` | スターブロック出現率 |
| `diamond_spawn_rate_percent` | ダイヤブロック出現率 |
| `item_clear_above_rate_percent` | Cアイテム出現率 |
| `item_unify_colors_rate_percent` | Rアイテム出現率 |
| `item_starify_screen_rate_percent` | Kアイテム出現率 |
| `color_count` | 色ブロックの色数 |
| `color_cluster_rate_percent` | 色ブロックの結合しやすさ |
| `bomb_spawn_rate_percent` | ボム出現率 |
| `bomb_fuse_ms` | ボム起爆までの時間 |
| `attack_blocks_per_rock` | 岩1個に必要な攻撃力(妨害の受け方) |
| `attack_rocks_per_wave_max` | 1ウェーブの岩上限 |
| `attack_blocks_per_bomb` | ボム1個に必要な攻撃力 |
| `attack_bombs_per_wave_max` | 1ウェーブのボム上限 |
| `attack_bomb_ratio_percent` | 攻撃力のボム化比率 |
| `chain_vanish_interval_ms` | 連鎖消滅のインターバル |

## 対象外(理由)

- `music_enabled`/`se_enabled`/`music_volume_percent`/`se_volume_percent`: 音楽・SEはAIに無関係
- `dodge_recovery_ms`/`move_cooldown_ms`: 反応速度系。今回は対策しない方針のため対象外
- `field_width`: 対戦相手全員が同じ盤面幅でないと対戦が成立しない。AIだけ別の幅にはできない(技術的制約)
- `debug_log_enabled`: デバッグ機能、AIには無関係
- `last_course_depth_m`: 前回選択したコースの記録(設定ではなく状態)
- `rewind_stock_max`: 対戦中は巻き戻し自体が無効化されている(spec.md 12.5)ため無意味

## 技術設計

### なぜAI専用値もネットワーク越しに配る必要があるか

対戦は非同期方式(spec.md 12.3)で、参加者全員が自分の手元で他の参加者(AI含む)の盤面コピーをシミュレートする。AIを実際に操作するのはホストだけ(#300)だが、ゲストも`games[i]`としてAIの盤面コピーを持ち、ゴースト表示(#301)や決着判定に使う。AI専用パラメータで地形やアイテム配分が変わる以上、ゲスト側のコピーもホストと同じパラメータで生成しないと、ゴーストの位置が実際のホスト側の盤面とズレる。

したがって既存の`attack_blocks_per_rock`等と同じ扱いで、AI専用値も`BattleConfig`(ホストが一方的に配る設定一式)に含める。`Settings`(ローカル永続化)と`BattleConfig`(通信で配る値)の両方に20項目を追加する。

### Game生成の分岐

既存の`new_game_from_battle_config(seed, config)`(battle.rs)は変更しない。新規に`new_ai_game_from_battle_config(seed, config)`を追加し、内部で`config.ai_*`を使って同じ手順でGameを組み立てる(既存関数のコピーに近い形で構わない)。

呼び出し元の分岐:
- `lobby.rs::start_ai_battle`(#296、通信なしAI対戦): `ai_games`の生成を`new_ai_game_from_battle_config`に差し替えるだけ
- `lobby.rs::battle_from_room`(#274、通信ありのN人対戦。人間+AI混在): 現在は`games`全員を`new_game_from_battle_config`で生成している。room内インデックスに対応する`streams[i-1]`が`None`(AI枠。#300の判定と同じ)なら`new_ai_game_from_battle_config`、`Some`なら既存関数、という分岐を追加する

### 設定値の永続化・同期

- `Settings`(settings.rs): 20フィールド追加。既定値は人間用と同じ値から始める(既存の`*_DEFAULT`定数を再利用してよい。AI専用の初期値を別に用意する必要はない)
- `BattleConfig`(net.rs): 20フィールド追加。`BattleConfig::from_settings`に反映を追加
- `game/mod.rs`: 既存の`set_attack_blocks_per_rock`等のsetterはGameが「今の自分が岩用/AI用のどちらの値を持つか」を区別しないので、setter自体の追加は不要。呼び出し側でどちらの値を渡すかだけの話

## UI設計

新規画面(`Screen`に1バリアント追加。仮に`AiSettings`)。既存の設定画面(`draw_settings`)と同じ構造で別関数`draw_ai_settings`を新設し、20項目を一覧表示・上下キーでカーソル移動・左右キー(または+/-)で増減する既存パラダイムをそのまま使う。

入り口:
- ロビー画面(`Screen::NetworkLobby`、`LobbyPhase::Discovering`)でI/Dキーの説明の近くに新規キー(未使用のキーを選定)を割り当て、`AiSettings`へ遷移
- AI対戦の人数選択画面(`LobbyPhase::SelectingAiOpponentCount`)からも同じキーで遷移できるようにする

戻り先: 開いた場所(ロビーまたは人数選択画面)へ戻る。既存の一時停止中の設定画面呼び出し(#23)が同種の「元の画面に戻る」パターンを持っているはずなので、それに合わせる。

## 影響ファイル(見積り)

- `src/settings.rs`: 20フィールド追加(構造体・Default・JSON手書きシリアライズ・パース・テスト)
- `src/net.rs`: `BattleConfig`に20フィールド追加、`from_settings`、ラウンドトリップテスト
- `src/battle.rs`: `new_ai_game_from_battle_config`新設
- `src/lobby.rs`: `start_ai_battle`/`battle_from_room`の生成分岐、新規画面への遷移キー処理(2箇所)
- `src/app/screens.rs`: `Screen`新規バリアント、tick関数
- `src/app/settings_menu.rs`: adjust系関数20個
- `src/ui/render.rs`: `draw_ai_settings`、選択項目の列挙、テスト
- `src/input.rs`: 新規キー割り当て・`InputAction`
- `docs/spec.md`: 新セクション

## 実装規模と進め方

#304(3項目+プロトコル拡張)より明らかに大きい(20項目+新規画面+入り口2箇所)。1回の実装委譲でやり切るとレビューが困難になるため、2フェーズに分けて進める。

- フェーズA(データ層): Settings/BattleConfig/Game生成分岐。UIは無く、既存のデバッグ経路(例えば一時的なテストコードやデバッグショートカット)で値が正しく反映されることをテストで確認する
- フェーズB(UI層): 新規画面・入力・入り口2箇所

各フェーズごとに四谷方式(Opus developer委譲→検証→release build→commit/push)を回す。
