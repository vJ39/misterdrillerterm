//! 1行テキスト編集の状態と操作(#270)。
//!
//! プレイヤー名入力画面のために作ったが、UI(ratatui)・ネットワークのどちらにも
//! 依存しない純粋なロジックだけを置く。カーソル位置・選択範囲は「文字(char)単位」で
//! 持ち、日本語や絵文字のようなマルチバイト文字でも1文字=1インデックスになる。
//! バイト長が問題になるのは`insert_char`の上限判定だけ(探索パケットの表示名フィールドは
//! 固定バイト長のため。net.rsの`PLAYER_NAME_LEN`)。

/// 1行テキストの編集状態。
pub struct TextEditState {
    /// 編集中の内容。バイト列ではなく文字の並びで持つ。
    text: Vec<char>,
    /// カーソル位置(文字単位。`0..=text.len()`。`text.len()`は末尾=最後の文字の後ろ)。
    cursor: usize,
    /// 選択の起点(文字単位)。`Some(anchor)`のとき選択範囲は`anchor`と`cursor`の間で、
    /// どちらが小さいかは決まっていない(参照側は`selection_range`で正規化して受け取る)。
    selection_anchor: Option<usize>,
}

impl TextEditState {
    /// 初期値`initial`を入れた状態を作る。カーソルは末尾に置く(そのまま打ち足せる)。
    pub fn new(initial: &str) -> Self {
        let text: Vec<char> = initial.chars().collect();
        let cursor = text.len();
        Self {
            text,
            cursor,
            selection_anchor: None,
        }
    }

    /// 現在の内容。
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// 描画用の文字列(文字単位でカーソル・選択範囲と突き合わせられるようにする)。
    pub fn chars(&self) -> &[char] {
        &self.text
    }

    /// カーソル位置(文字単位)。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 選択範囲を`(start, end)`(`start <= end`)へ正規化して返す。選択が無い、または
    /// 幅0(起点とカーソルが同じ位置)なら`None`。
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// カーソルを1文字左へ。`extend`がtrueなら選択範囲を伸ばし、falseなら選択を解除する。
    pub fn move_left(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// カーソルを1文字右へ。末尾では動かない。
    pub fn move_right(&mut self, extend: bool) {
        self.prepare_move(extend);
        if self.cursor < self.text.len() {
            self.cursor += 1;
        }
    }

    /// カーソルを行頭へ(Home)。
    pub fn move_to_start(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = 0;
    }

    /// カーソルを行末へ(End)。
    pub fn move_to_end(&mut self, extend: bool) {
        self.prepare_move(extend);
        self.cursor = self.text.len();
    }

    /// 全選択(Ctrl+A / Cmd+A)。起点を先頭、カーソルを末尾に置く。
    pub fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// カーソル位置へ1文字入れる。選択があればまず選択範囲を消してその位置へ入れる
    /// (=選択の置き換え)。改行・制御文字は受け付けない。置き換え後の全体のUTF-8バイト長が
    /// `max_bytes`を超える場合は、選択も内容も変えずに何もしない。入れられたかどうかを
    /// 返す(呼び出し側はfalseを無視してよい)。
    pub fn insert_char(&mut self, c: char, max_bytes: usize) -> bool {
        if c.is_control() {
            return false;
        }
        // 上限の判定は消す前に済ませる(消してから戻すと、選択の向き=起点とカーソルの
        // どちらが前かが入れ替わってしまう)。選択の置き換えでは、消える分だけ余地が増える。
        let total_bytes: usize = self.text.iter().map(|ch| ch.len_utf8()).sum();
        let selected_bytes: usize = self
            .selection_range()
            .map(|(start, end)| self.text[start..end].iter().map(|ch| ch.len_utf8()).sum())
            .unwrap_or(0);
        if total_bytes - selected_bytes + c.len_utf8() > max_bytes {
            return false;
        }
        self.take_selection();
        self.text.insert(self.cursor, c);
        self.cursor += 1;
        true
    }

    /// Backspace。選択があれば選択範囲を消し、無ければカーソル直前の1文字を消す。
    pub fn backspace(&mut self) {
        if self.take_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.text.remove(self.cursor);
    }

    /// Delete。選択があれば選択範囲を消し、無ければカーソル直後の1文字を消す。
    pub fn delete_forward(&mut self) {
        if self.take_selection() {
            return;
        }
        if self.cursor >= self.text.len() {
            return;
        }
        self.text.remove(self.cursor);
    }

    /// カーソル移動の前処理。`extend`がtrueなら(まだ無ければ)現在位置を選択の起点に据え、
    /// falseなら選択を解除する。
    fn prepare_move(&mut self, extend: bool) {
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
    }

    /// 選択範囲があれば消す。カーソルは消した位置へ移し、選択は解除する。
    /// 消したかどうかを返す(選択が無ければ何もせずfalse)。
    fn take_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        self.text.drain(start..end);
        self.cursor = start;
        self.selection_anchor = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表示名フィールドと同じ16バイト上限(テストの読みやすさのため別名で置く)。
    const MAX: usize = 16;

    #[test]
    fn new_puts_the_cursor_at_the_end_of_the_initial_text() {
        let state = TextEditState::new("abc");
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor(), 3);
        assert_eq!(state.selection_range(), None);
    }

    #[test]
    fn moving_the_cursor_never_goes_out_of_range() {
        let mut state = TextEditState::new("ab");
        state.move_left(false);
        state.move_left(false);
        state.move_left(false);
        assert_eq!(state.cursor(), 0, "先頭より左へは出ない");
        state.move_right(false);
        state.move_right(false);
        state.move_right(false);
        assert_eq!(state.cursor(), 2, "末尾より右へは出ない");
    }

    #[test]
    fn home_and_end_jump_to_both_ends() {
        let mut state = TextEditState::new("abcd");
        state.move_to_start(false);
        assert_eq!(state.cursor(), 0);
        state.move_to_end(false);
        assert_eq!(state.cursor(), 4);
    }

    #[test]
    fn extending_starts_the_selection_at_the_current_cursor_and_keeps_the_anchor() {
        let mut state = TextEditState::new("abcd");
        // 末尾(4)から左へ2回。起点は4のまま、カーソルは2へ。
        state.move_left(true);
        state.move_left(true);
        assert_eq!(state.selection_range(), Some((2, 4)));
        assert_eq!(state.cursor(), 2);
        // 起点は動かさずカーソルだけ戻すと、選択は幅0になりNoneへ。
        state.move_right(true);
        state.move_right(true);
        assert_eq!(state.selection_range(), None);
        assert_eq!(state.cursor(), 4);
    }

    #[test]
    fn shift_home_selects_up_to_the_line_start() {
        let mut state = TextEditState::new("abcd");
        state.move_to_start(true);
        assert_eq!(state.selection_range(), Some((0, 4)));
    }

    #[test]
    fn shift_end_selects_up_to_the_line_end() {
        let mut state = TextEditState::new("abcd");
        state.move_to_start(false);
        state.move_to_end(true);
        assert_eq!(state.selection_range(), Some((0, 4)));
    }

    #[test]
    fn moving_without_extend_clears_the_selection() {
        let mut state = TextEditState::new("abcd");
        state.select_all();
        assert_eq!(state.selection_range(), Some((0, 4)));
        state.move_left(false);
        assert_eq!(state.selection_range(), None);
    }

    #[test]
    fn inserting_a_char_puts_it_at_the_cursor() {
        let mut state = TextEditState::new("ac");
        state.move_left(false);
        assert!(state.insert_char('b', MAX));
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor(), 2);
    }

    #[test]
    fn inserting_a_char_replaces_the_selection() {
        let mut state = TextEditState::new("abcd");
        state.move_left(true);
        state.move_left(true);
        assert_eq!(state.selection_range(), Some((2, 4)));
        assert!(state.insert_char('Z', MAX));
        assert_eq!(state.text(), "abZ");
        assert_eq!(state.cursor(), 3);
        assert_eq!(state.selection_range(), None);
    }

    #[test]
    fn inserting_rejects_control_chars() {
        let mut state = TextEditState::new("ab");
        assert!(!state.insert_char('\n', MAX));
        assert!(!state.insert_char('\t', MAX));
        assert_eq!(state.text(), "ab", "制御文字は入らない");
    }

    #[test]
    fn inserting_stops_at_the_byte_limit() {
        // ちょうど収まる境界と、1バイト超える境界の両方を見る。
        let mut state = TextEditState::new("123456789012345");
        assert_eq!(state.text().len(), 15);
        assert!(state.insert_char('6', MAX), "16バイト目はちょうど収まる");
        assert_eq!(state.text().len(), 16);
        assert!(
            !state.insert_char('7', MAX),
            "17バイト目は超えるので入らない"
        );
        assert_eq!(state.text(), "1234567890123456");
    }

    #[test]
    fn a_multibyte_char_that_does_not_fit_is_rejected_whole() {
        // 全角1文字=3バイト。14バイト入っている状態には入らない(14+3=17>16)。
        let mut state = TextEditState::new("12345678901234");
        assert_eq!(state.text().len(), 14);
        assert!(!state.insert_char('あ', MAX));
        assert_eq!(state.text(), "12345678901234");
        // 1文字消して13バイトになれば、3バイトがちょうど収まる(13+3=16)。
        state.backspace();
        assert!(state.insert_char('あ', MAX));
        assert_eq!(state.text(), "1234567890123あ");
        assert_eq!(state.text().len(), 16);
        // 上限ちょうどのため、1バイト文字でももう入らない。
        assert!(!state.insert_char('5', MAX));
        assert_eq!(state.text().len(), 16);
    }

    #[test]
    fn a_rejected_insert_keeps_the_selection_as_it_was() {
        // 上限ちょうどの状態で選択中に文字を入れようとして入らなかった場合、選択だけが
        // 消えた状態にならない(利用者から見て「選択が勝手に外れた」ようにならない)。
        // 全角5文字(15バイト)+半角1文字(1バイト)=16バイトで上限ちょうど。
        let mut state = TextEditState::new("あいうえおa");
        assert_eq!(state.text().len(), 16);
        // 末尾の半角1文字だけを選ぶ。
        state.move_left(true);
        assert_eq!(state.selection_range(), Some((5, 6)));
        // 1バイト消しても4バイト文字は入らない(16-1+4=19>16)。
        assert!(!state.insert_char('🙂', MAX));
        assert_eq!(state.text(), "あいうえおa", "内容が変わっていない");
        assert_eq!(state.selection_range(), Some((5, 6)), "選択が保たれている");
        assert_eq!(state.cursor(), 5);
        // 同じ選択のまま、1バイト文字であれば置き換えられる(16-1+1=16)。
        assert!(state.insert_char('b', MAX));
        assert_eq!(state.text(), "あいうえおb");
        assert_eq!(state.selection_range(), None);
        assert_eq!(state.cursor(), 6);
    }

    #[test]
    fn backspace_removes_the_char_before_the_cursor() {
        let mut state = TextEditState::new("abc");
        state.backspace();
        assert_eq!(state.text(), "ab");
        assert_eq!(state.cursor(), 2);
    }

    #[test]
    fn backspace_at_the_line_start_does_nothing() {
        let mut state = TextEditState::new("abc");
        state.move_to_start(false);
        state.backspace();
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn backspace_after_select_all_clears_everything() {
        let mut state = TextEditState::new("Player-1a2b");
        state.select_all();
        state.backspace();
        assert_eq!(state.text(), "");
        assert_eq!(state.cursor(), 0);
        assert_eq!(state.selection_range(), None);
    }

    #[test]
    fn delete_forward_removes_the_char_after_the_cursor() {
        let mut state = TextEditState::new("abc");
        state.move_to_start(false);
        state.delete_forward();
        assert_eq!(state.text(), "bc");
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn delete_forward_at_the_line_end_does_nothing() {
        let mut state = TextEditState::new("abc");
        state.delete_forward();
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor(), 3);
    }

    #[test]
    fn delete_forward_removes_the_selection() {
        let mut state = TextEditState::new("abcd");
        state.move_to_start(false);
        state.move_right(true);
        state.move_right(true);
        assert_eq!(state.selection_range(), Some((0, 2)));
        state.delete_forward();
        assert_eq!(state.text(), "cd");
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn the_cursor_moves_per_char_over_multibyte_text() {
        // 日本語3文字(9バイト)でも、カーソルは文字単位で3つぶんしか動かない。
        let mut state = TextEditState::new("あいう");
        assert_eq!(state.cursor(), 3);
        state.move_left(false);
        assert_eq!(state.cursor(), 2);
        state.backspace();
        assert_eq!(state.text(), "あう");
        assert_eq!(state.chars(), ['あ', 'う']);
    }

    #[test]
    fn select_all_over_an_empty_text_is_an_empty_selection() {
        let mut state = TextEditState::new("");
        state.select_all();
        assert_eq!(state.selection_range(), None, "幅0の選択はNone");
        state.backspace();
        assert_eq!(state.text(), "");
        state.delete_forward();
        assert_eq!(state.text(), "");
    }
}
