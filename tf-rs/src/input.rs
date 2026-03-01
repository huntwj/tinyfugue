//! Line editor — the input buffer and cursor-based editing operations.
//!
//! Corresponds to the `keybuf` / `keyboard_pos` globals and the
//! `do_kbdel`, `do_kbword`, `do_kbmatch`, `handle_input_string`
//! functions in `keyboard.c`.
//!
//! ## Design
//!
//! The buffer is a `Vec<char>` so that cursor movement and editing work in
//! Unicode characters rather than bytes.  [`LineEditor::pos`] is always a
//! valid char index (`0..=buffer.len()`).
//!
//! The kill ring is a single slot (matching TF's simple behaviour); a full
//! multi-entry ring is a future enhancement.

// ── LineEditor ────────────────────────────────────────────────────────────────

/// A readline-style line editor backed by a `Vec<char>`.
///
/// All positions are in Unicode scalar values (chars), not bytes.
#[derive(Debug, Clone)]
pub struct LineEditor {
    buffer: Vec<char>,
    /// Cursor position (0 = before first char, `buffer.len()` = after last).
    pub pos: usize,
    /// When `true`, typed characters overwrite rather than insert.
    pub insert_mode: bool,
    /// Extra characters treated as word-constituents (`wordpunct` TF variable).
    pub wordpunct: String,
    /// Last killed text, available for yanking.
    kill_ring: Vec<char>,
    /// Cached UTF-8 representation of `buffer`.  Rebuilt lazily when `dirty`.
    cached_text: String,
    /// True when `buffer` has been modified since the last cache rebuild.
    dirty: bool,
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            pos: 0,
            insert_mode: true,
            wordpunct: String::new(),
            kill_ring: Vec::new(),
            cached_text: String::new(),
            dirty: false,
        }
    }

    // ── Buffer access ─────────────────────────────────────────────────────────

    /// Current content as a borrowed `&str`.
    ///
    /// The underlying `String` is rebuilt only when the buffer has changed
    /// since the last call, making this allocation-free on repeat calls
    /// between edits.  Takes `&mut self` to allow lazy cache rebuild.
    pub fn text_ref(&mut self) -> &str {
        if self.dirty {
            self.cached_text.clear();
            for &ch in &self.buffer {
                self.cached_text.push(ch);
            }
            self.dirty = false;
        }
        &self.cached_text
    }

    /// Current content as an owned `String`.
    ///
    /// Prefers the cache; only allocates when the buffer has changed.
    pub fn text(&mut self) -> String {
        self.text_ref().to_owned()
    }

    /// Mark the cached text as stale.  Called after every buffer mutation.
    #[inline]
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The buffer contents as a char slice (direct access, no allocation).
    pub fn chars(&self) -> &[char] {
        &self.buffer
    }

    /// Number of characters in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Consume and return the buffer contents, resetting the editor to empty.
    pub fn take_line(&mut self) -> String {
        let line = self.text_ref().to_owned();
        self.buffer.clear();
        self.pos = 0;
        self.cached_text.clear();
        self.dirty = false;
        line
    }

    /// Replace the entire buffer with `text`, placing the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.buffer = text.chars().collect();
        self.pos = self.buffer.len();
        self.mark_dirty();
    }

    // ── Insertion ─────────────────────────────────────────────────────────────

    /// Insert `ch` at the cursor, advancing the cursor.
    ///
    /// In overwrite mode the character under the cursor is replaced (unless
    /// the cursor is at the end of the buffer, in which case it appends).
    pub fn insert_char(&mut self, ch: char) {
        if self.insert_mode || self.pos == self.buffer.len() {
            self.buffer.insert(self.pos, ch);
        } else {
            self.buffer[self.pos] = ch;
        }
        self.pos += 1;
        self.mark_dirty();
    }

    /// Insert `s` at the cursor, advancing the cursor by `s.chars().count()`.
    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert_char(ch);
        }
    }

    // ── Deletion ──────────────────────────────────────────────────────────────

    /// Delete the character immediately before the cursor (backspace).
    /// Returns `true` if a character was deleted.
    pub fn delete_before(&mut self) -> bool {
        if self.pos == 0 {
            return false;
        }
        self.pos -= 1;
        self.buffer.remove(self.pos);
        self.mark_dirty();
        true
    }

    /// Delete the character under the cursor (forward delete).
    /// Returns `true` if a character was deleted.
    pub fn delete_at(&mut self) -> bool {
        if self.pos >= self.buffer.len() {
            return false;
        }
        self.buffer.remove(self.pos);
        self.mark_dirty();
        true
    }

    /// Delete from `from` to `to` (exclusive), treating negative-direction
    /// deletions correctly.  Mirrors `do_kbdel(place)` in the C source.
    ///
    /// If `to < from`, the region `[to, from)` is deleted and the cursor
    /// moves to `to`.  If `to > from`, the region `[from, to)` is deleted.
    /// Returns `true` if any characters were deleted.
    pub fn delete_region(&mut self, from: usize, to: usize) -> bool {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        let hi = hi.min(self.buffer.len());
        if lo >= hi {
            return false;
        }
        self.buffer.drain(lo..hi);
        self.pos = self.pos.min(lo).min(self.buffer.len());
        self.mark_dirty();
        true
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

    /// Move the cursor left by up to `n` characters.
    pub fn move_left(&mut self, n: usize) {
        self.pos = self.pos.saturating_sub(n);
    }

    /// Move the cursor right by up to `n` characters.
    pub fn move_right(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buffer.len());
    }

    /// Move the cursor to the start of the buffer.
    pub fn move_home(&mut self) {
        self.pos = 0;
    }

    /// Move the cursor to the end of the buffer.
    pub fn move_end(&mut self) {
        self.pos = self.buffer.len();
    }

    /// Move the cursor to an absolute position, clamped to `[0, len]`.
    pub fn move_to(&mut self, pos: usize) {
        self.pos = pos.min(self.buffer.len());
    }

    // ── Word navigation ───────────────────────────────────────────────────────

    /// Find the boundary of a word starting at `start`, moving in direction
    /// `dir` (+1 = forward, -1 = backward).
    ///
    /// Mirrors `do_kbword(start, dir)` in `keyboard.c`.
    /// Returns the char index of the far edge of the word.
    pub fn word_boundary(&self, start: usize, dir: i32) -> usize {
        let len = self.buffer.len();
        let stop: i64 = if dir < 0 { -1 } else { len as i64 };
        let mut place = start.min(len) as i64 - if dir < 0 { 1 } else { 0 };

        // Skip non-word characters.
        while place != stop && !self.is_word_char(place as usize) {
            place += dir as i64;
        }
        // Skip word characters.
        while place != stop && self.is_word_char(place as usize) {
            place += dir as i64;
        }

        if dir < 0 {
            (place + 1).max(0) as usize
        } else {
            place.min(len as i64) as usize
        }
    }

    /// Move the cursor to the start of the next word (forward).
    pub fn move_word_forward(&mut self) {
        let new_pos = self.word_boundary(self.pos, 1);
        self.pos = new_pos;
    }

    /// Move the cursor to the start of the previous word (backward).
    pub fn move_word_backward(&mut self) {
        let new_pos = self.word_boundary(self.pos, -1);
        self.pos = new_pos;
    }

    // ── Brace matching ────────────────────────────────────────────────────────

    /// Find the matching bracket/brace/parenthesis for the character at or
    /// after `start`.  Returns the index of the matching character, or
    /// `None` if no match is found.
    ///
    /// Mirrors `do_kbmatch(start)` in `keyboard.c`.
    pub fn find_match(&self, start: usize) -> Option<usize> {
        const OPENS: &str = "([{";
        const CLOSES: &str = ")]}";

        let buf = &self.buffer;
        let mut place = start;

        // Find the first bracket at or after `start`.
        let (open_ch, close_ch, dir) = loop {
            if place >= buf.len() {
                return None;
            }
            let ch = buf[place];
            if let Some(i) = OPENS.find(ch) {
                break (ch, CLOSES.chars().nth(i).unwrap(), 1i32);
            }
            if let Some(i) = CLOSES.find(ch) {
                break (ch, OPENS.chars().nth(i).unwrap(), -1i32);
            }
            place += 1;
        };

        let mut depth: i32 = 0;
        let stop: i64 = if dir > 0 { buf.len() as i64 } else { -1 };
        let mut i = place as i64;
        loop {
            let c = buf[i as usize];
            if c == open_ch {
                depth += 1;
            } else if c == close_ch {
                depth -= 1;
                if depth == 0 {
                    return Some(i as usize);
                }
            }
            i += dir as i64;
            if i == stop {
                break;
            }
        }
        None
    }

    // ── Kill / yank ───────────────────────────────────────────────────────────

    /// Kill (cut) from the cursor to the end of the buffer.
    pub fn kill_to_end(&mut self) {
        self.kill_ring = self.buffer[self.pos..].to_vec();
        self.buffer.truncate(self.pos);
        self.mark_dirty();
    }

    /// Kill (cut) from the start of the buffer to the cursor.
    pub fn kill_to_start(&mut self) {
        self.kill_ring = self.buffer[..self.pos].to_vec();
        self.buffer.drain(..self.pos);
        self.pos = 0;
        self.mark_dirty();
    }

    /// Kill the word forward of the cursor.
    pub fn kill_word_forward(&mut self) {
        let end = self.word_boundary(self.pos, 1);
        self.kill_ring = self.buffer[self.pos..end].to_vec();
        self.buffer.drain(self.pos..end);
        self.mark_dirty();
    }

    /// Kill the word backward of the cursor.
    pub fn kill_word_backward(&mut self) {
        let start = self.word_boundary(self.pos, -1);
        self.kill_ring = self.buffer[start..self.pos].to_vec();
        self.buffer.drain(start..self.pos);
        self.pos = start;
        self.mark_dirty();
    }

    /// Yank (paste) the kill ring contents at the cursor.
    pub fn yank(&mut self) {
        let yanked = self.kill_ring.clone();
        for ch in yanked {
            self.buffer.insert(self.pos, ch);
            self.pos += 1;
        }
        self.mark_dirty();
    }

    /// Content of the kill ring.
    pub fn kill_ring_text(&self) -> String {
        self.kill_ring.iter().collect()
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn is_word_char(&self, idx: usize) -> bool {
        let ch = self.buffer[idx];
        ch.is_alphanumeric() || self.wordpunct.contains(ch)
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Insert ────────────────────────────────────────────────────────────────

    #[test]
    fn insert_builds_text() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.pos, 5);
    }

    #[test]
    fn insert_at_middle() {
        let mut ed = LineEditor::new();
        ed.insert_str("hllo");
        ed.move_left(3);
        ed.insert_char('e');
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.pos, 2);
    }

    #[test]
    fn overwrite_mode() {
        let mut ed = LineEditor::new();
        ed.insert_mode = false;
        ed.insert_str("hello");
        ed.move_home();
        ed.insert_char('H');
        assert_eq!(ed.text(), "Hello");
        assert_eq!(ed.pos, 1);
    }

    #[test]
    fn overwrite_at_end_appends() {
        let mut ed = LineEditor::new();
        ed.insert_mode = false;
        ed.insert_str("hi");
        ed.insert_char('!'); // pos is at end, should append
        assert_eq!(ed.text(), "hi!");
    }

    // ── Delete ────────────────────────────────────────────────────────────────

    #[test]
    fn delete_before_cursor() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        assert!(ed.delete_before());
        assert_eq!(ed.text(), "hell");
        assert_eq!(ed.pos, 4);
    }

    #[test]
    fn delete_before_at_start_returns_false() {
        let mut ed = LineEditor::new();
        ed.insert_str("hi");
        ed.move_home();
        assert!(!ed.delete_before());
    }

    #[test]
    fn delete_at_cursor() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        ed.move_home();
        assert!(ed.delete_at());
        assert_eq!(ed.text(), "ello");
        assert_eq!(ed.pos, 0);
    }

    #[test]
    fn delete_at_end_returns_false() {
        let mut ed = LineEditor::new();
        ed.insert_str("hi");
        assert!(!ed.delete_at());
    }

    #[test]
    fn delete_region_forward() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.move_home();
        ed.delete_region(0, 6);
        assert_eq!(ed.text(), "world");
    }

    #[test]
    fn delete_region_backward() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        // delete_region(5, 2) → deletes [2,5)
        ed.delete_region(5, 2);
        assert_eq!(ed.text(), "he");
    }

    // ── Movement ──────────────────────────────────────────────────────────────

    #[test]
    fn move_left_right() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        ed.move_left(3);
        assert_eq!(ed.pos, 2);
        ed.move_right(1);
        assert_eq!(ed.pos, 3);
    }

    #[test]
    fn move_clamped() {
        let mut ed = LineEditor::new();
        ed.insert_str("hi");
        ed.move_left(100);
        assert_eq!(ed.pos, 0);
        ed.move_right(100);
        assert_eq!(ed.pos, 2);
    }

    #[test]
    fn move_home_end() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        ed.move_home();
        assert_eq!(ed.pos, 0);
        ed.move_end();
        assert_eq!(ed.pos, 5);
    }

    #[test]
    fn take_line_resets() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello");
        let line = ed.take_line();
        assert_eq!(line, "hello");
        assert_eq!(ed.text(), "");
        assert_eq!(ed.pos, 0);
    }

    #[test]
    fn set_text_places_cursor_at_end() {
        let mut ed = LineEditor::new();
        ed.set_text("world");
        assert_eq!(ed.pos, 5);
        assert_eq!(ed.text(), "world");
    }

    // ── Word navigation ───────────────────────────────────────────────────────

    #[test]
    fn word_forward() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.move_home();
        ed.move_word_forward(); // should land at space or 'w'
        // "hello" is 5 chars; forward from 0 lands after word at 5
        assert_eq!(ed.pos, 5);
    }

    #[test]
    fn word_backward() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        // cursor at end (pos=11)
        ed.move_word_backward(); // should land at start of "world" = 6
        assert_eq!(ed.pos, 6);
    }

    #[test]
    fn wordpunct_included_in_word() {
        let mut ed = LineEditor::new();
        ed.wordpunct = "_".to_owned();
        ed.insert_str("foo_bar baz");
        ed.move_home();
        ed.move_word_forward(); // "foo_bar" is all one word
        assert_eq!(ed.pos, 7);
    }

    // ── Brace matching ────────────────────────────────────────────────────────

    #[test]
    fn match_parens() {
        let mut ed = LineEditor::new();
        ed.insert_str("(hello)");
        assert_eq!(ed.find_match(0), Some(6));
    }

    #[test]
    fn match_nested() {
        let mut ed = LineEditor::new();
        ed.insert_str("((a))");
        assert_eq!(ed.find_match(0), Some(4));
        assert_eq!(ed.find_match(1), Some(3));
    }

    #[test]
    fn match_backward() {
        let mut ed = LineEditor::new();
        ed.insert_str("(hello)");
        // Closing paren at index 6
        assert_eq!(ed.find_match(6), Some(0));
    }

    #[test]
    fn no_match_returns_none() {
        let mut ed = LineEditor::new();
        ed.insert_str("(hello");
        assert_eq!(ed.find_match(0), None);
    }

    #[test]
    fn match_skips_to_first_bracket() {
        let mut ed = LineEditor::new();
        ed.insert_str("abc(def)");
        // Start scan from 0; first bracket is at 3
        assert_eq!(ed.find_match(0), Some(7));
    }

    // ── Kill / yank ───────────────────────────────────────────────────────────

    #[test]
    fn kill_to_end() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.move_home();
        ed.move_right(5);
        ed.kill_to_end();
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.kill_ring_text(), " world");
    }

    #[test]
    fn kill_to_start() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.move_left(5);
        ed.kill_to_start();
        assert_eq!(ed.text(), "world");
        assert_eq!(ed.kill_ring_text(), "hello ");
    }

    #[test]
    fn kill_word_forward() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.move_home();
        ed.kill_word_forward();
        assert_eq!(ed.kill_ring_text(), "hello");
        assert_eq!(ed.text(), " world");
    }

    #[test]
    fn kill_word_backward() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.kill_word_backward();
        assert_eq!(ed.kill_ring_text(), "world");
        assert_eq!(ed.text(), "hello ");
    }

    #[test]
    fn yank_restores_killed_text() {
        let mut ed = LineEditor::new();
        ed.insert_str("hello world");
        ed.move_home();
        ed.kill_word_forward(); // kills "hello"
        ed.move_end();
        ed.yank();
        assert_eq!(ed.text(), " worldhello");
    }

    // ── Unicode ───────────────────────────────────────────────────────────────

    #[test]
    fn unicode_insert_and_delete() {
        let mut ed = LineEditor::new();
        ed.insert_str("héllo"); // 5 chars, but 6 UTF-8 bytes
        assert_eq!(ed.len(), 5);
        assert_eq!(ed.pos, 5);
        ed.delete_before();
        assert_eq!(ed.text(), "héll");
    }

    #[test]
    fn unicode_word_nav() {
        let mut ed = LineEditor::new();
        ed.insert_str("café world");
        ed.move_home();
        ed.move_word_forward();
        assert_eq!(ed.pos, 4); // "café" is 4 chars
    }
}
