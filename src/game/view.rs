//! 描画向けgetter群(#262)。`src/ui/render.rs`等が参照する、盤面・演出状態を読み取るだけの
//! 副作用のないgetter(と、その内部専用ヘルパー)をまとめる。

use super::*;

impl Game {
    /// GameOverダイアログの現在の選択項目。
    pub fn game_over_selection(&self) -> GameOverChoice {
        self.game_over_selection
    }

    /// 現在、無敵(ミス無効)かどうか。
    pub fn is_invincible(&self) -> bool {
        self.invincible
    }

    /// 無敵によって回避されたミスの累計回数。
    pub fn misses_averted(&self) -> u32 {
        self.misses_averted
    }

    /// 残っている巻き戻しの使用回数。HUD表示・GameOverダイアログのヒントが参照する。
    pub fn rewind_stock(&self) -> u8 {
        self.rewind_stock
    }

    /// 現在の巻き戻しストック上限(設定画面の`rewind_stock_max`)。`0`なら機能OFF。
    pub fn rewind_stock_max(&self) -> u8 {
        self.rewind_stock_max
    }

    /// 巻き戻しを開始できるか。ストックが残っていて、かつプレイ中(昇天演出中を含む)か
    /// GameOver中であること(一時停止中・クリア後は開始できない)。履歴が1つでもあるか
    /// どうかは`Game`の外(`rewind::RewindHistory`)が別途判定する。
    pub fn can_start_rewind(&self) -> bool {
        self.rewind_stock > 0 && matches!(self.status, GameStatus::Playing | GameStatus::GameOver)
    }

    /// 描画側が使う、移動補間の進捗(0.0=直前位置にいる, 1.0=現在位置に到達済み)。
    pub fn move_anim_progress(&self) -> f32 {
        (self.render_anim_elapsed / self.render_anim_duration_secs).clamp(0.0, 1.0)
    }

    /// 描画側が使う、移動補間の起点(直前の論理位置)。
    pub fn render_prev_position(&self) -> (usize, usize) {
        self.render_prev_position
    }

    /// 描画側が使う、直近の重力ティックで実際に1マス落下した各セルの(移動後の位置,
    /// 移動前の位置)一覧。ブロック落下のピクセル単位補間描画に使い、次のティックが
    /// 実行されるまでこのティックの内容を保持し続ける。
    pub fn recently_moved_blocks(&self) -> &[BlockMove] {
        &self.last_block_moves
    }

    /// 描画側が使う、指定セルの消滅フラッシュ演出の進捗(0.0=フラッシュ開始直後、
    /// 1.0=演出完了直前)。フラッシュ中のセルのみ`Some`を返し、落下ブロックの到着待ち
    /// (`delay`>0)のセルは`None`を返す。
    pub fn vanish_flash_progress(&self, pos: board::Pos) -> Option<f32> {
        self.recently_vanished
            .iter()
            .find(|e| e.pos == pos && e.delay.is_zero())
            .map(|e| {
                let total = e.total.as_secs_f32().max(0.001);
                (1.0 - e.remaining.as_secs_f32() / total).clamp(0.0, 1.0)
            })
    }

    /// 描画側が使う、消滅は確定したがまだフラッシュに入っていない(落下ブロックの到着
    /// 待ちの)セルの、消滅直前の種類。着地と同一tickで4連結自動消滅した場合、盤面は既に
    /// Emptyのため描画側は表示すべきグリフを盤面から読めない。待機中はここから消滅直前の
    /// 種類を取得して、到着するまで元の見た目のまま描き続ける。
    pub fn pending_vanish_kind(&self, pos: board::Pos) -> Option<Cell> {
        self.recently_vanished
            .iter()
            .find(|e| e.pos == pos && !e.delay.is_zero())
            .map(|e| e.kind)
    }

    /// 指定セルの消滅演出が終わるまでの残り時間(ms、デバッグログ用)。フラッシュ開始
    /// 待ちも含めた合計を返す。対象でなければ`None`。
    fn recently_vanished_flash_remaining_ms(&self, pos: board::Pos) -> Option<u64> {
        self.recently_vanished
            .iter()
            .find(|e| e.pos == pos)
            .map(|e| (e.delay + e.remaining).as_millis() as u64)
    }

    /// 落下ブロック補間描画(`draw_falling_blocks`)が、着地先セルが盤面上で既にEmptyに
    /// なっている場面に遭遇したことを記録する。デバッグログ無効時は何もしない。
    pub fn log_render_fallback(&self, to: board::Pos, from: board::Pos, resolved: Option<Cell>) {
        if let Some(log) = &self.debug_log {
            let resolved_kind = resolved.map(|k| format!("{k:?}"));
            log.log_render_fallback(
                self.frame_counter,
                to,
                from,
                resolved_kind.as_deref(),
                self.block_fall_progress(),
                self.effective_block_fall_tick_ms(),
                self.recently_vanished_flash_remaining_ms(to),
            );
        }
    }

    /// 描画側が使う、指定セルのボム爆発・炎演出の進捗(0.0=爆発直後、1.0=演出完了直前)と
    /// 爆心地からの距離。対象でなければ`None`を返す。
    pub fn explosion_flash_progress(&self, pos: board::Pos) -> Option<(f32, u8)> {
        let flash = Duration::from_millis(BOMB_EXPLOSION_FLASH_MS)
            .as_secs_f32()
            .max(0.001);
        self.recently_exploded
            .iter()
            .find(|&&(p, _, _)| p == pos)
            .map(|&(_, remaining, tier)| {
                (
                    (1.0 - remaining.as_secs_f32() / flash).clamp(0.0, 1.0),
                    tier,
                )
            })
    }

    /// 描画側が使う、ブロック落下ティックの進捗(0.0=直前のティック直後, 1.0=次のティック
    /// が来る直前)。`recently_moved_blocks`と組み合わせて移動前後を滑らかに補間する。
    pub fn block_fall_progress(&self) -> f32 {
        let tick_secs = self.effective_block_fall_tick_ms().max(1) as f32 / 1000.0;
        (self.fall_tick_accum.as_secs_f32() / tick_secs).clamp(0.0, 1.0)
    }

    /// 押し潰しの「潰れた」演出が表示中かどうか(GameOverオーバーレイの表示可否判定にも使う)。
    /// 最後のライフでのミスも同じ演出を経てからGameOverになるため、GameOverへ遷移した
    /// 時点ではこれは偽になっており、そのままオーバーレイを表示できる(#257)。
    pub fn crush_flash_active(&self) -> bool {
        self.crush_flash_remaining > Duration::ZERO || self.ascending_remaining.is_some()
    }

    /// チェックポイント(100mごと)到達演出が表示中なら、到達した深度(m)を返す。
    /// 描画側(render.rs)がバナー表示に使う。
    pub fn checkpoint_flash_depth_m(&self) -> Option<usize> {
        if self.checkpoint_flash_remaining > Duration::ZERO {
            Some(self.checkpoint_flash_depth_m)
        } else {
            None
        }
    }

    /// 掘削アニメーション中の描画フレーム(9章)。掘削演出中でなければ`None`、演出中は
    /// `DRILL_ANIM_FRAME_MS`ごとに`true`/`false`を切り替えて返す(描画側が2フレームを
    /// 交互に選ぶ)。
    pub fn drilling_frame(&self) -> Option<bool> {
        if self.drill_flash_remaining <= Duration::ZERO {
            return None;
        }
        let elapsed_ms =
            DRILL_ANIM_MS.saturating_sub(self.drill_flash_remaining.as_millis() as u64);
        Some((elapsed_ms / DRILL_ANIM_FRAME_MS.max(1)).is_multiple_of(2))
    }

    /// 「わ〜!」スライダー演出中(横滑り段階のみ)かどうか。描画側がスプライトを横滑り
    /// させる判断に使う。硬直(Recovering)段階では滑りが止まっているため`false`を返すが、
    /// `is_input_frozen`相当のフリーズ自体はそちらも継続する。
    pub fn is_dodge_sliding(&self) -> bool {
        self.dodge_stage == DodgeStage::Sliding
    }

    /// 「わ〜!」スライダー演出の横滑り進捗(0.0=開始直後、1.0=スライダー完了直前)。
    /// スライダー中でなければ0.0を返す。
    pub fn dodge_slide_progress(&self) -> f32 {
        if self.dodge_stage != DodgeStage::Sliding {
            return 0.0;
        }
        let total = DODGE_SLIDE_MS as f32 / 1000.0;
        if total <= 0.0 {
            return 1.0;
        }
        (1.0 - self.dodge_stage_remaining.as_secs_f32() / total).clamp(0.0, 1.0)
    }

    /// 「天に召される」演出の進捗(0.0=演出開始直後、1.0=演出完了直前)。演出中でなければ
    /// 0.0を返す。描画側(render.rs)がキャラのスプライトを少しずつ上へドリフトさせる。
    pub fn ascend_progress(&self) -> f32 {
        let Some(remaining) = self.ascending_remaining else {
            return 0.0;
        };
        let total = CRUSH_ASCEND_MS as f32 / 1000.0;
        if total <= 0.0 {
            return 1.0;
        }
        (1.0 - remaining.as_secs_f32() / total).clamp(0.0, 1.0)
    }

    /// 指定セルが現在「震えている」(支えを失い、落下開始までの猶予期間中)かどうか
    /// (描画用)。
    pub fn is_cell_shaking(&self, row: usize, col: usize) -> bool {
        self.gravity_state.is_shaking((row, col))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::clear_board;
    use super::*;
    use crate::constants::{FRAME_INTERVAL_MS, SHAKE_TICKS};

    #[test]
    fn can_start_rewind_is_true_only_while_playing_or_game_over_and_with_stock_left() {
        let mut game = Game::new(1);

        game.status = GameStatus::Playing;
        assert!(game.can_start_rewind(), "プレイ中は起動できる");

        // 昇天演出中(is_dying)もプレイ中扱いなので起動できる(押し潰された直後に
        // 押して戻る、が主要な使い方)。
        game.ascending_remaining = Some(Duration::from_millis(100));
        assert!(game.is_dying(), "前提: 昇天演出中");
        assert!(game.can_start_rewind(), "昇天演出中も起動できる");
        game.ascending_remaining = None;

        game.status = GameStatus::GameOver;
        assert!(game.can_start_rewind(), "GameOver中も起動できる");

        game.status = GameStatus::Paused;
        assert!(!game.can_start_rewind(), "一時停止中は起動できない");

        game.status = GameStatus::Cleared;
        assert!(!game.can_start_rewind(), "クリア後は起動できない");

        game.status = GameStatus::Playing;
        game.rewind_stock = 0;
        assert!(!game.can_start_rewind(), "ストックが無ければ起動できない");
    }

    #[test]
    fn misses_averted_counts_every_averted_miss() {
        let mut game = Game::new(4);
        game.set_invincible(true);

        for expected in 1..=3 {
            game.player.oxygen = 1.0;
            game.update(Duration::from_secs(1));
            assert_eq!(game.misses_averted(), expected);
        }
    }

    #[test]
    fn game_over_selection_defaults_to_back_to_title_and_toggles() {
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1));
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);
        assert_eq!(game.game_over_selection(), GameOverChoice::BackToTitle);

        game.toggle_game_over_selection();
        assert_eq!(game.game_over_selection(), GameOverChoice::Revive);

        game.toggle_game_over_selection();
        assert_eq!(game.game_over_selection(), GameOverChoice::BackToTitle);
    }

    #[test]
    fn checkpoint_flash_clears_after_checkpoint_flash_ms_elapses() {
        let mut game = Game::new(91);
        game.player.row =
            crate::constants::CHECKPOINT_STEP_M + crate::constants::CHECKPOINT_SAFE_ZONE_M - 1;
        game.player.facing = Direction::Down;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        game.try_drill();
        game.update(Duration::from_millis(FALL_TICK_MS));
        assert_eq!(game.checkpoint_flash_depth_m(), Some(100));

        game.update(Duration::from_millis(
            crate::constants::CHECKPOINT_FLASH_MS + 10,
        ));
        assert_eq!(
            game.checkpoint_flash_depth_m(),
            None,
            "表示時間が過ぎたら演出は終わっているはず"
        );
    }

    #[test]
    fn auto_vanished_cells_show_a_vanish_flash_that_expires_after_block_vanish_flash_ms() {
        // 自動消滅したセルは消滅直後にフラッシュ演出の対象になり、
        // BLOCK_VANISH_FLASH_MS経過後に対象から外れることを確認する。
        let mut game = Game::new(12);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        game.board.rows[998][0] = Cell::Color(ColorKind::Red);
        game.board.rows[998][1] = Cell::Color(ColorKind::Red);
        game.board.rows[998][2] = Cell::Color(ColorKind::Red);
        game.board.rows[999][3] = Cell::Color(ColorKind::Red); // 最深行=常に支持

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        for col in 0..=3 {
            assert!(
                game.vanish_flash_progress((999, col)).is_some(),
                "消滅直後のセル(999,{col})はフラッシュ演出の対象になっているはず"
            );
        }
        assert!(
            game.vanish_flash_progress((0, 0)).is_none(),
            "無関係なセルはフラッシュ演出の対象ではないはず"
        );

        game.update(Duration::from_millis(
            crate::constants::BLOCK_VANISH_FLASH_MS + 10,
        ));
        assert!(
            game.vanish_flash_progress((999, 0)).is_none(),
            "BLOCK_VANISH_FLASH_MS経過後はフラッシュ演出が終わっているはず"
        );
    }

    #[test]
    fn melted_star_cell_also_shows_a_vanish_flash() {
        // スター溶解による消滅も、自動消滅と同様にフラッシュ演出の対象になることを確認する。
        let mut game = Game::new(13);
        clear_board(&mut game);
        game.player.row = 999; // 最深行=常に支持される安定した足場にできる
        game.player.col = 5;
        game.board.rows[999][3] = Cell::Rock { hits: 0 }; // 最深行=常に支持
        game.board.rows[998][3] = Cell::Star { visible_ms: 0 }; // 岩の上に乗った、支えのあるスター

        game.update(Duration::from_millis(
            crate::constants::STAR_VISIBLE_GRACE_MS as u64
                + crate::constants::STAR_MELT_DURATION_MS as u64,
        ));

        assert_eq!(
            game.board.cell(998, 3),
            Cell::Empty,
            "溶け切ったスターは消えているはず"
        );
        assert!(
            game.vanish_flash_progress((998, 3)).is_some(),
            "溶けて消えたスターもフラッシュ演出の対象になっているはず"
        );
    }

    #[test]
    fn recently_moved_blocks_and_progress_track_the_latest_gravity_tick() {
        // 実際に1マス落下したtickの直後は、その(移動後の位置, 移動前の位置)が
        // recently_moved_blocksに記録され、block_fall_progressはそのtickの開始直後を
        // 表す小さな値になっていることを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        // 深度による落下速度スケーリングの影響を受けないよう、プレイヤーは
        // 深度0m相当(等倍速)の浅い位置に置く。
        game.player.row = 1;
        game.player.col = 5;
        game.board.rows[0][3] = Cell::Color(ColorKind::Red);

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        let moves = game.recently_moved_blocks();
        assert_eq!(
            moves,
            &[((1, 3), (0, 3))],
            "揺れが明けて1マス落下した直後のはず"
        );
        assert!(
            game.block_fall_progress() < 0.2,
            "ティック開始直後なのでprogressは小さいはず: {}",
            game.block_fall_progress()
        );

        // 次のtickまでの間、時間経過とともにprogressが増える。
        game.update(Duration::from_millis(FALL_TICK_MS / 2));
        assert!(
            game.block_fall_progress() > 0.3,
            "半分近く経過すればprogressも増えるはず: {}",
            game.block_fall_progress()
        );
    }

    #[test]
    fn free_fall_move_animation_duration_matches_player_fall_tick_ms_not_the_fixed_default() {
        // 自由落下の見た目補間は、横移動用の固定の短い時間(MOVE_ANIM_DURATION_MS)では
        // なく、実際のplayer_fall_tick_msぶんかけて行われることを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 10;
        game.set_player_fall_tick_ms(400); // MOVE_ANIM_DURATION_MS(100ms)よりずっと長い

        // ちょうど1回ぶんの自由落下tickを発生させる。
        game.update(Duration::from_millis(410));
        assert_eq!(game.player.row, 11, "1マス落下しているはず");
        assert_eq!(
            game.move_anim_progress(),
            0.0,
            "落下tick直後は補間がまだ始まったばかりのはず"
        );

        // 次のtick(400ms後)がまだ来ない150ms経過時点でも、補間が完了していないはず
        // (固定100msのままなら、ここで既に1.0=完了してしまう)。
        game.update(Duration::from_millis(150));
        assert!(
            game.move_anim_progress() < 1.0,
            "player_fall_tick_ms(400ms)に合わせた補間ならまだ完了していないはず: {}",
            game.move_anim_progress()
        );
    }

    #[test]
    fn oxygen_miss_also_activates_the_flash_effect() {
        // 酸素切れ死亡でも押し潰しと同じ「潰れた」フラッシュ演出が起きる。
        let mut game = Game::new(30);
        game.player.oxygen = 1.0;

        game.update(Duration::from_secs(1)); // 酸素切れでミス(押し潰しではない)

        assert!(game.crush_flash_active());
    }

    #[test]
    fn crush_flash_decays_to_inactive_after_crush_flash_duration() {
        let mut game = Game::new(31);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        // SHAKE_TICKSぶんの揺れ+落下の1ティックで押し潰しが発生する
        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.crush_flash_active(), "押し潰し直後は演出が有効なはず");

        // ライフが残っているので「天に召される」演出(CRUSH_ASCEND_MS)が動く。
        // これが終わるまでは演出が有効なままのはず。
        game.update(Duration::from_millis(crate::constants::CRUSH_FLASH_MS + 10));
        assert!(
            game.crush_flash_active(),
            "天に召される演出(CRUSH_ASCEND_MS)がまだ終わっていないはず"
        );

        // CRUSH_ASCEND_MSぶん時間を進めると演出は終わる
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert!(
            !game.crush_flash_active(),
            "CRUSH_ASCEND_MS経過後は演出が終わっているはず"
        );
    }

    #[test]
    fn drilling_frame_is_none_before_any_drill_input() {
        let game = Game::new(60);
        assert_eq!(
            game.drilling_frame(),
            None,
            "掘削していなければアニメーションフレームは無いはず"
        );
    }

    #[test]
    fn drilling_frame_alternates_then_clears_after_drill_anim_duration() {
        // 掘削入力直後はアニメーションフレームが交互に切り替わり、DRILL_ANIM_MS
        // 経過後は通常表示(None)に戻ることを確認する。
        let mut game = Game::new(61);
        clear_board(&mut game);
        game.player.row = 5;
        game.player.col = 5;

        game.try_drill();
        assert_eq!(
            game.drilling_frame(),
            Some(true),
            "掘削直後は最初のフレームのはず"
        );

        game.update(Duration::from_millis(DRILL_ANIM_FRAME_MS));
        assert_eq!(
            game.drilling_frame(),
            Some(false),
            "1フレーム経過で切り替わるはず"
        );

        game.update(Duration::from_millis(
            DRILL_ANIM_MS - DRILL_ANIM_FRAME_MS + 10,
        ));
        assert_eq!(
            game.drilling_frame(),
            None,
            "DRILL_ANIM_MS経過後は通常表示に戻るはず"
        );
    }

    #[test]
    fn new_game_starts_with_move_animation_already_settled() {
        // 開始直後にいきなり(0,0)相当からアニメーションしてしまわないことの確認。
        let game = Game::new(32);
        assert_eq!(game.move_anim_progress(), 1.0);
        assert_eq!(game.render_prev_position(), game.player.position());
    }

    #[test]
    fn lateral_move_starts_interpolation_from_the_previous_position_then_settles() {
        let mut game = Game::new(33);
        // 開始直後の上2行は常にEmptyなので、直下に足場を置いて横移動できる状態にする。
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Rock { hits: 0 };
        let before = game.player.position();

        let events = game.try_move_right();
        assert!(events.is_empty());
        assert_ne!(
            game.player.position(),
            before,
            "前提: 実際に移動しているはず"
        );

        assert_eq!(
            game.render_prev_position(),
            before,
            "補間の起点は移動前の位置のはず"
        );
        assert!(
            game.move_anim_progress() < 1.0,
            "移動直後は補間がまだ完了していないはず"
        );

        game.update(Duration::from_millis(
            crate::constants::MOVE_ANIM_DURATION_MS + 10,
        ));
        assert_eq!(
            game.move_anim_progress(),
            1.0,
            "MOVE_ANIM_DURATION_MS経過後は補間が完了しているはず"
        );
    }

    // -----------------------------------------------------------------------
    // 落下tick間隔を遅くした際の「落下→消滅」演出
    // -----------------------------------------------------------------------

    /// テスト用ヘルパー: 盤面を3行に切り詰め、最深行(row2)を常に支持される足場にした上で、
    /// 「(0,0)の色ブロックが2マス落下し、着地先(2,0)で(2,1)(2,2)(2,3)と4連結して消滅する」
    /// 盤面を作る。落下tick間隔だけをパラメータで変えて、演出の破綻を比較できるようにする。
    fn landing_vanish_game(block_fall_tick_ms: u64) -> Game {
        let mut game = Game::new(1);
        game.set_block_fall_tick_ms(block_fall_tick_ms);
        game.board.rows.truncate(3);
        clear_board(&mut game);
        game.player.row = 0;
        game.player.col = 5; // 落下グループから十分離す
        game.board.rows[0][0] = Cell::Color(ColorKind::Red);
        for col in 1..=3 {
            game.board.rows[2][col] = Cell::Color(ColorKind::Red);
        }
        game
    }

    /// テスト用ヘルパー: `landing_vanish_game`の盤面を1フレーム(33ms)ずつ進め、
    /// 落下ブロックが着地して4連結消滅するまで到達させる。消滅の判定には静止セル(2,1)を
    /// 使う(このセルは自分では動かないため、Emptyになるのは4連結消滅した時だけ)。
    fn advance_to_landing_vanish(game: &mut Game) {
        let frame = Duration::from_millis(FRAME_INTERVAL_MS);
        for _ in 0..400 {
            game.update(frame);
            if game.board.cell(2, 1) == Cell::Empty {
                return;
            }
        }
        panic!("着地と同一tickでの4連結消滅が起きなかった");
    }

    #[test]
    fn landing_block_keeps_its_look_until_the_fall_interpolation_finishes_at_any_tick_rate() {
        // 落下tick間隔を遅くすると、消滅フラッシュの寿命が1tickぶんの落下補間より短くなり、
        // 落下中のブロックが空中で消えてしまう。どのtick間隔でも「補間が終わるまでは消滅
        // 直前の見た目を保持している」ことを確認する。
        let frame = Duration::from_millis(FRAME_INTERVAL_MS);
        for tick_ms in [25u64, 150, 300, 450, 600] {
            let mut game = landing_vanish_game(tick_ms);
            let mut vanished = false;
            let mut checked_frames = 0;

            for _ in 0..400 {
                game.update(frame);
                // 着地セルが盤面から消えている(4連結消滅済み)のに、まだ落下補間が
                // 続いている間は、消滅直前の見た目を保持していなければならない。
                let still_falling = game
                    .recently_moved_blocks()
                    .iter()
                    .any(|&(to, _)| to == (2, 0));
                if still_falling && game.board.cell(2, 0) == Cell::Empty {
                    assert!(
                        game.pending_vanish_kind((2, 0)).is_some(),
                        "tick={tick_ms}ms: 落下補間中(progress={})に着地セルの見た目が失われている",
                        game.block_fall_progress()
                    );
                    checked_frames += 1;
                }
                if game.board.cell(2, 1) == Cell::Empty {
                    vanished = true;
                    if !still_falling {
                        break;
                    }
                }
            }

            assert!(
                vanished,
                "tick={tick_ms}ms: 着地と同一tickでの4連結消滅が起きなかった"
            );
            // 1フレーム(33ms)より短いtickでは落下tickがフレーム内で完結してしまい、
            // 「補間の途中」をフレーム境界で観測できない。それ以外は必ず観測できるはず。
            if tick_ms > FRAME_INTERVAL_MS {
                assert!(
                    checked_frames > 0,
                    "tick={tick_ms}ms: 落下補間中のフレームを1つも検証できていない"
                );
            }
        }
    }

    #[test]
    fn vanish_flash_starts_only_after_the_falling_block_has_arrived() {
        // 着地tickの瞬間にフラッシュを始めると、まだ空中にいるブロックの着地先が先に光る。
        // 着地セル・静止セルとも、落下補間が終わる(次のtickが来る)まではフラッシュに
        // 入らないことを確認する。
        let mut game = landing_vanish_game(300);
        advance_to_landing_vanish(&mut game);

        let frame = Duration::from_millis(FRAME_INTERVAL_MS);
        let mut waited_frames = 0;
        while game.vanish_flash_progress((2, 0)).is_none() {
            assert!(
                game.pending_vanish_kind((2, 0)).is_some(),
                "フラッシュ前の着地セルは待機中(消滅直前の見た目)であるはず"
            );
            assert!(
                game.vanish_flash_progress((2, 1)).is_none(),
                "一緒に消える静止セルも、落下ブロックが着くまでは光り始めないはず"
            );
            game.update(frame);
            waited_frames += 1;
            assert!(
                waited_frames < 40,
                "フラッシュが始まらないまま待ち続けている"
            );
        }

        assert!(
            waited_frames >= 2,
            "tick=300msなら着地から数フレームは待機するはず(実際は{waited_frames}フレーム)"
        );
        assert!(
            game.vanish_flash_progress((2, 0)).unwrap() < 0.3,
            "フラッシュは始まったばかりのはず"
        );
        assert!(
            game.vanish_flash_progress((2, 1)).is_some(),
            "静止セルも同じタイミングでフラッシュに入るはず"
        );
        assert!(
            game.pending_vanish_kind((2, 0)).is_none(),
            "フラッシュに入ったら待機中ではなくなるはず"
        );
    }
}
