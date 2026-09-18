//! デバッグショートカット系ロジック(#256/#257等)。デバッグ画面から盤面・プレイヤー
//! 状態を直接書き換える各種操作(揺れ時間/落下速度調整・ライフ/酸素回復・頭上クリア・
//! 色統一・スター化・フレーム通し番号取得)をまとめる。

use super::*;

impl Game {
    /// `update()`が呼ばれるたびに1増えるフレーム通し番号。ブロック状態遷移ログの各行と
    /// 突き合わせるための識別子として画面に表示する。
    pub fn debug_frame(&self) -> u64 {
        self.frame_counter
    }

    /// デバッグ: 揺れ時間(ブロックが支えを失ってから実際に落下し始めるまでの時間)を
    /// `DEBUG_SHAKE_DURATION_STEP_MS`ぶん増減する。`longer`がtrueなら長く(遅く反応)、
    /// falseなら短く(速く反応、0まで)する。
    pub fn debug_adjust_shake_duration(&mut self, longer: bool) {
        self.shake_duration_ms = if longer {
            (self.shake_duration_ms + DEBUG_SHAKE_DURATION_STEP_MS).min(DEBUG_SHAKE_DURATION_MS_MAX)
        } else {
            self.shake_duration_ms
                .saturating_sub(DEBUG_SHAKE_DURATION_STEP_MS)
        };
    }

    /// デバッグ: ブロック落下速度を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する。
    /// `faster`がtrueならtick間隔を短くして速く、falseなら長くして遅くする。
    pub fn debug_adjust_block_fall_speed(&mut self, faster: bool) {
        self.block_fall_tick_ms = adjust_fall_tick_ms(self.block_fall_tick_ms, faster);
    }

    /// デバッグ: プレイヤー自由落下速度を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する。
    pub fn debug_adjust_player_fall_speed(&mut self, faster: bool) {
        self.player_fall_tick_ms = adjust_fall_tick_ms(self.player_fall_tick_ms, faster);
    }

    /// デバッグ: ライフを1増やす(`LIVES_MAX`でクランプ)。Playing中のみ有効。
    /// 「天に召される」演出中(`is_dying`)も無効にする。演出の完了時にライフを減らして
    /// 復活/GameOverを分岐するため、途中でライフを増やすと本来GameOverになる場面が
    /// 復活側へ倒れてしまうため(#257)。
    pub fn debug_add_life(&mut self) {
        if self.status == GameStatus::Playing && !self.is_dying() {
            self.player.lives = (self.player.lives + 1).min(LIVES_MAX);
        }
    }

    /// デバッグ: 酸素(AIR)を100%まで回復する。Playing中のみ有効。
    pub fn debug_fill_air(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.oxygen = crate::constants::OXYGEN_MAX;
        }
    }

    /// デバッグ: プレイヤーより浅い(画面上で上にある)行を全てEmptyにする。Playing中のみ
    /// 有効。AIR(酸素カプセル)・アイテムブロックは消滅させず残す。死亡時の頭上クリアと
    /// 100mごとのチェックポイント到達時も、この同じ関数を呼び出して統一する。
    pub fn debug_clear_above_player(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        // 4連結自動消滅と同じ消滅フラッシュ演出を出す。AIR・アイテムブロック(C/R/K)は
        // 消さずに残す。
        //
        // 画面外(`entry_row`より浅い)に残っていたAIR/アイテムは、画面のすぐ外側
        // (`just_off_screen_row`)を起点にさらに浅い側へ積み上げ、以後の重力ティックで
        // 自然に画面内へ落ちてくるようにする(画面内へ直接置くといきなり現れて見える)。
        // 既に画面内にあった分はそのまま動かさない。
        let width = self.board.width();
        let entry_row = self.player.row.saturating_sub(PLAYER_SCREEN_ROWS_ABOVE);
        let just_off_screen_row = entry_row.saturating_sub(1);
        let mut off_screen_by_col: Vec<Vec<Cell>> = vec![Vec::new(); width];
        let mut cleared = Vec::new();
        for row in 0..self.player.row {
            for (col, bucket) in off_screen_by_col.iter_mut().enumerate() {
                let cell = self.board.cell(row, col);
                if matches!(cell, Cell::Oxygen | Cell::Item(_)) {
                    if row < entry_row {
                        bucket.push(cell);
                        self.board.set(row, col, Cell::Empty);
                    }
                } else if cell != Cell::Empty {
                    cleared.push(((row, col), cell));
                    self.board.set(row, col, Cell::Empty);
                }
            }
        }
        self.note_vanished_cells(cleared, Duration::ZERO);

        // 列ごとに、画面のすぐ外側(just_off_screen_row)を起点に、元の深さ順(浅い方が先)
        // を保ったまま浅い側(まだ画面に入らない側)へ空いているマスを探して詰め直す。
        for (col, cells) in off_screen_by_col.into_iter().enumerate() {
            let mut row = just_off_screen_row;
            for cell in cells {
                while self.board.cell(row, col) != Cell::Empty && row > 0 {
                    row -= 1;
                }
                if self.board.cell(row, col) != Cell::Empty {
                    break; // 置き場所が無ければそれ以上は諦める(起こりにくい極端なケース)
                }
                self.board.set(row, col, cell);
                if row == 0 {
                    break;
                }
                row -= 1;
            }
        }
    }

    /// デバッグ: プレイヤー付近(上下`DEBUG_UNIFY_COLORS_RANGE_ROWS`行)の色ブロックを
    /// ランダムに選んだ2色だけへ揃える。Playing中のみ有効。
    ///
    /// 抽選にはOS乱数ではなくゲーム開始時のシードから作った`self.rng`を使う。同じシード・
    /// 同じ入力列なら盤面が完全に再現されるようにするため。
    pub fn debug_unify_nearby_colors(&mut self) -> Vec<GameEvent> {
        if self.status != GameStatus::Playing {
            return Vec::new();
        }
        let rng = &mut self.rng;

        let all = ColorKind::ALL;
        let first = all[rng.random_range(0..all.len())];
        let second_offset = 1 + rng.random_range(0..all.len() - 1);
        let second =
            all[(all.iter().position(|&c| c == first).unwrap() + second_offset) % all.len()];

        let start_row = self
            .player
            .row
            .saturating_sub(DEBUG_UNIFY_COLORS_RANGE_ROWS);
        let end_row = (self.player.row + DEBUG_UNIFY_COLORS_RANGE_ROWS)
            .min(self.board.depth_rows().saturating_sub(1));
        for row in start_row..=end_row {
            for col in 0..self.board.width() {
                if matches!(self.board.cell(row, col), Cell::Color(_)) {
                    let chosen = if rng.random_bool(0.5) { first } else { second };
                    self.board.set(row, col, Cell::Color(chosen));
                }
            }
        }

        // 重力ティックの外から色配置を直接書き換えたため、塊(連結グループ)の境界が
        // 変わっている。まだ揺れ猶予中(落下し始めていない)の古い揺れ状態は引きずらず、
        // 次の重力ティックで結合関係を一から作り直させる。ただし既に揺れが明けて連続
        // 落下中の塊まで揺れ直させると、押した瞬間にフリーズしたように見えるため対象外。
        //
        // 塗り替えで新たに4連結以上になった箇所はここでは消さない。消滅判定は通常の
        // 重力ティック(支えを失って落下・着地した場合のみ)に委ねる。
        self.gravity_state.reset_shake_progress(self.shake_ticks());

        Vec::new()
    }

    /// デバッグ: 画面内(プレイヤー位置から上下`STAR_VISIBLE_RANGE_ROWS`行)にあるXブロック・
    /// ダイヤブロックを、揺れ中のセルを除き全てスターブロックへ変える。Playing中のみ有効。
    pub fn debug_starify_visible_screen(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        let start_row = self
            .player
            .row
            .saturating_sub(crate::constants::STAR_VISIBLE_RANGE_ROWS);
        let end_row = (self.player.row + crate::constants::STAR_VISIBLE_RANGE_ROWS)
            .min(self.board.depth_rows().saturating_sub(1));
        for row in start_row..=end_row {
            for col in 0..self.board.width() {
                if matches!(self.board.cell(row, col), Cell::Rock { .. } | Cell::Diamond)
                    && !self.gravity_state.is_shaking((row, col))
                {
                    self.board.set(row, col, Cell::Star { visible_ms: 0 });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::clear_board;
    use super::*;
    use crate::constants::SHAKE_TICKS;

    #[test]
    fn debug_clear_above_player_moves_off_screen_oxygen_capsules_to_just_outside_the_screen() {
        // 画面外のAIRは画面内へ直接テレポートさせず、画面のすぐ外側
        // (just_off_screen_row = entry_row - 1)まで移動させ、以後の重力ティックで
        // 自然に画面内へ落ちてくるようにする(いきなり現れないため)。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][5] = Cell::Oxygen; // 画面外(entry_row=46より浅い)
        game.board.rows[10][6] = Cell::Rock { hits: 2 }; // 比較用: AIR以外は通常通り消える

        game.debug_clear_above_player();

        assert_eq!(
            game.board.cell(10, 5),
            Cell::Empty,
            "画面外に残っていたAIRは元の位置には残らないはず"
        );
        let entry_row = game.player.row - crate::constants::PLAYER_SCREEN_ROWS_ABOVE;
        let just_off_screen_row = entry_row - 1;
        assert_eq!(
            game.board.cell(just_off_screen_row, 5),
            Cell::Oxygen,
            "AIRは画面のすぐ外側まで移動しているはず(画面内にいきなり現れない)"
        );
        assert_eq!(
            game.board.cell(entry_row, 5),
            Cell::Empty,
            "この時点ではまだ画面内には現れていないはず"
        );
        assert_eq!(
            game.board.cell(10, 6),
            Cell::Empty,
            "AIR以外は通常通り消えるはず"
        );
    }

    #[test]
    fn debug_clear_above_player_moves_off_screen_item_blocks_to_just_outside_the_screen_preserving_order()
     {
        // アイテムブロックもAIRと同じく削除されない。同じ列に複数ある場合、元の深さ順
        // (浅い方が先)を保ったまま画面のすぐ外側からさらに浅い側へ詰め直す
        // (画面内にはまだ現れない)。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][5] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[11][5] = Cell::Item(ItemEffect::UnifyColors);
        game.board.rows[12][5] = Cell::Item(ItemEffect::StarifyScreen);
        game.board.rows[10][6] = Cell::Rock { hits: 2 }; // 比較用: アイテム以外は通常通り消える

        game.debug_clear_above_player();

        let entry_row = game.player.row - crate::constants::PLAYER_SCREEN_ROWS_ABOVE;
        let just_off_screen_row = entry_row - 1;
        assert_eq!(
            game.board.cell(just_off_screen_row, 5),
            Cell::Item(ItemEffect::ClearAbove),
            "元々一番浅かったRアイテムが画面のすぐ外側の先頭に詰め直されるはず"
        );
        assert_eq!(
            game.board.cell(just_off_screen_row - 1, 5),
            Cell::Item(ItemEffect::UnifyColors),
            "Cアイテムがその次(さらに浅い側)に詰め直されるはず"
        );
        assert_eq!(
            game.board.cell(just_off_screen_row - 2, 5),
            Cell::Item(ItemEffect::StarifyScreen),
            "Kアイテムがさらにその次に詰め直されるはず"
        );
        assert_eq!(
            game.board.cell(entry_row, 5),
            Cell::Empty,
            "この時点ではまだ画面内には現れていないはず"
        );
        assert_eq!(
            game.board.cell(10, 5),
            Cell::Empty,
            "アイテムは元の画面外の位置には残らないはず"
        );
        assert_eq!(
            game.board.cell(10, 6),
            Cell::Empty,
            "アイテム以外は通常通り消えるはず"
        );
    }

    #[test]
    fn debug_clear_above_player_shows_the_same_vanish_flash_as_auto_vanish() {
        // 頭上クリアでの消滅も、4連結自動消滅と同じフラッシュ演出を出す。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][6] = Cell::Rock { hits: 2 };

        game.debug_clear_above_player();

        assert!(
            game.vanish_flash_progress((10, 6)).is_some(),
            "4連結自動消滅と同じ消滅フラッシュが出るはず"
        );
    }

    #[test]
    fn debug_fill_air_restores_oxygen_to_max_while_playing() {
        let mut game = Game::new(72);
        game.player.oxygen = 1.0;

        game.debug_fill_air();

        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
    }

    #[test]
    fn debug_fill_air_does_nothing_when_not_playing() {
        let mut game = Game::new_with_lives(72, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1)); // 酸素切れ+ライフ1でミスさせる
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);

        game.debug_fill_air();

        assert_eq!(game.player.oxygen, 0.0, "GameOver中は酸素を回復しない");
    }

    #[test]
    fn debug_starify_visible_screen_converts_rock_and_diamond_within_range_to_stars() {
        let mut game = Game::new(73);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[990][3] = Cell::Rock { hits: 2 };
        game.board.rows[995][4] = Cell::Diamond;

        game.debug_starify_visible_screen();

        assert!(matches!(game.board.cell(990, 3), Cell::Star { .. }));
        assert!(matches!(game.board.cell(995, 4), Cell::Star { .. }));
    }

    #[test]
    fn debug_starify_visible_screen_does_not_convert_cells_outside_the_visible_range() {
        let mut game = Game::new(73);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[900][3] = Cell::Rock { hits: 0 }; // STAR_VISIBLE_RANGE_ROWSより外

        game.debug_starify_visible_screen();

        assert!(
            matches!(game.board.cell(900, 3), Cell::Rock { .. }),
            "画面外のブロックは変化しないはず"
        );
    }

    #[test]
    fn debug_starify_visible_screen_leaves_color_and_oxygen_cells_untouched() {
        let mut game = Game::new(73);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[990][3] = Cell::Color(ColorKind::Red);
        game.board.rows[991][3] = Cell::Oxygen;

        game.debug_starify_visible_screen();

        assert!(matches!(
            game.board.cell(990, 3),
            Cell::Color(ColorKind::Red)
        ));
        assert!(matches!(game.board.cell(991, 3), Cell::Oxygen));
    }

    #[test]
    fn debug_starify_visible_screen_does_not_convert_shaking_cells() {
        // 揺れ中/落下中のブロックはスター化対象外。
        let mut game = Game::new(31);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[498][5] = Cell::Rock { hits: 0 }; // 直下(499)が空=支えなし

        game.update(Duration::from_millis(game.block_fall_tick_ms())); // 1ティックで揺れ開始(まだ落下しない)
        assert!(game.is_cell_shaking(498, 5), "テスト前提: 揺れ中であること");

        game.debug_starify_visible_screen();

        assert!(
            matches!(game.board.cell(498, 5), Cell::Rock { .. }),
            "揺れ中のセルは変化しないはず"
        );
    }

    #[test]
    fn debug_starify_visible_screen_does_nothing_when_not_playing() {
        let mut game = Game::new_with_lives(73, 1);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1)); // 酸素切れ+ライフ1でミスさせる
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);

        // GameOverになった後で改めて岩を置く(死亡時の頭上クリアの影響を受けずに、
        // starifyがGameOver中は何もしないことだけを確認するため)。
        game.board.rows[990][3] = Cell::Rock { hits: 0 };

        game.debug_starify_visible_screen();

        assert!(
            matches!(game.board.cell(990, 3), Cell::Rock { .. }),
            "GameOver中は変化しないはず"
        );
    }

    #[test]
    fn debug_add_life_does_nothing_while_the_ascending_sequence_plays() {
        // 演出の完了時にライフを減らして復活/GameOverを分岐するため、演出中に
        // ライフを増やせてしまうと本来のGameOverが復活側へ倒れる(#257)。
        let mut game = Game::new_with_lives(35, 1);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.is_dying(), "前提: 演出中");

        game.debug_add_life();

        assert_eq!(game.player.lives, 1, "演出中はライフが増えないはず");

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);
    }

    #[test]
    fn debug_frame_starts_at_zero_and_increments_once_per_update_call() {
        // refresh_debug_logを呼ばない限りdisk I/Oは発生しない(debug_logがNoneのまま
        // no-opになる)ので、通常のテストには影響しない。
        let mut game = Game::new(1);
        assert_eq!(game.debug_frame(), 0);
        game.update(Duration::from_millis(16));
        assert_eq!(game.debug_frame(), 1);
        game.update(Duration::from_millis(16));
        assert_eq!(game.debug_frame(), 2);
    }

    #[test]
    fn debug_unify_nearby_colors_repaints_to_exactly_two_colors_and_never_vanishes() {
        // ランダムな2色のみへ塗り替えるが、塗り替えによって新たに4連結以上になった箇所が
        // あっても即座には自動消滅させない(色の選択はシードから決まるため、シードを変えて
        // 十分な回数試行し両方の性質を確認する)。
        let mut saw_four_or_more_connected_and_intact = false;
        for trial in 0..300 {
            let mut game = Game::new(trial);
            clear_board(&mut game);
            game.player.row = 500;
            game.player.col = 5;
            game.board.rows[500][0] = Cell::Color(ColorKind::Red);
            game.board.rows[500][1] = Cell::Color(ColorKind::Blue);
            game.board.rows[500][2] = Cell::Color(ColorKind::Green);
            game.board.rows[500][3] = Cell::Color(ColorKind::Yellow);

            let events = game.debug_unify_nearby_colors();

            assert!(
                events.is_empty(),
                "塗り替えだけで自動消滅イベントは発生しないはず"
            );

            let mut colors_seen: Vec<ColorKind> = Vec::new();
            for c in 0..4 {
                if let Cell::Color(k) = game.board.cell(500, c)
                    && !colors_seen.contains(&k)
                {
                    colors_seen.push(k);
                }
            }
            assert!(
                colors_seen.len() <= 2,
                "2色より多い色が残っている: {colors_seen:?}"
            );
            for c in 0..4 {
                assert_ne!(
                    game.board.cell(500, c),
                    Cell::Empty,
                    "塗り替えただけで消滅してはいけない"
                );
            }

            if colors_seen.len() == 1 {
                saw_four_or_more_connected_and_intact = true;
                break;
            }
        }
        assert!(
            saw_four_or_more_connected_and_intact,
            "300回試行しても4連結が形成されるケースを確認できなかった"
        );
    }
}
