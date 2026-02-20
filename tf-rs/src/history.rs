//! Input history — storage, recall, and `^old^new` substitution.
//!
//! Corresponds to `history.c` / `history.h` (the input-history portion) in
//! the C source.  Command/world output history belongs to later phases.
//!
//! ## Recall modes
//!
//! | Mode | C call | Behaviour |
//! |------|--------|-----------|
//! | [`RecallMode::Exact`] | `recall_input(n, 0)` | Step n entries back/forward |
//! | [`RecallMode::Prefix`] | `recall_input(n, 1)` | Find nth entry whose prefix matches the saved line |
//! | [`RecallMode::Jump`]   | `recall_input(n, 2)` | Jump to oldest (n<0) or newest (n>0) saved entry |

use std::collections::VecDeque;

// ── RecallMode ────────────────────────────────────────────────────────────────

/// How [`InputHistory::recall`] searches the history buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallMode {
    /// Absolute step: move n entries back (n<0) or forward (n>0).
    Exact,
    /// Prefix search: find the nth entry whose text starts with the saved line.
    Prefix,
    /// Jump: n<0 → oldest entry, n>0 → newest saved entry.
    Jump,
}

// ── InputHistory ──────────────────────────────────────────────────────────────

/// Ring buffer of input lines, with recall and search.
///
/// The newest entry is at index 0.  The "current" position (index -1 in the C
/// convention) is the line the user is editing right now; it is not stored in
/// `entries` but saved in `saved_line` when scrolling into history begins.
#[derive(Debug, Clone)]
pub struct InputHistory {
    /// Past input lines, newest first.
    entries: VecDeque<String>,
    /// Maximum number of entries to keep.
    max_size: usize,

    // ── Recall state ──────────────────────────────────────────────────────
    /// `0` = at the live editing line; `n` = n entries back in history.
    recall_pos: usize,
    /// The live editing line, saved when we first scroll into history.
    saved_line: String,
}

impl InputHistory {
    /// Create an empty history with the given capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_size: max_size.max(1),
            recall_pos: 0,
            saved_line: String::new(),
        }
    }

    /// Number of entries stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ── Recording ─────────────────────────────────────────────────────────────

    /// Record `line` as the most recent input.
    ///
    /// Consecutive duplicate lines are collapsed to one entry.
    /// The recall position is reset to the live editing line.
    pub fn record(&mut self, line: &str) {
        self.recall_pos = 0;
        self.saved_line.clear();

        if line.is_empty() {
            return;
        }
        // Collapse consecutive duplicates.
        if self.entries.front().is_some_and(|e| e == line) {
            return;
        }
        self.entries.push_front(line.to_owned());
        while self.entries.len() > self.max_size {
            self.entries.pop_back();
        }
    }

    // ── Recall ────────────────────────────────────────────────────────────────

    /// Sync the saved current line before scrolling into history.
    ///
    /// Must be called with the current editing-buffer text whenever scrolling
    /// starts from the live line.  Mirrors `sync_input_hist()` in the C source.
    pub fn sync(&mut self, current: &str) {
        if self.recall_pos == 0 {
            self.saved_line = current.to_owned();
        }
    }

    /// Scroll through history by `n` steps (positive = older, negative = newer).
    ///
    /// Returns the text to load into the editor, or `None` if the boundary
    /// was already reached (caller should ring the bell).
    ///
    /// `current` is the live editing text, used for prefix matching and for
    /// saving when first entering history.
    pub fn recall(&mut self, n: i32, mode: RecallMode, current: &str) -> Option<&str> {
        self.sync(current);

        match mode {
            RecallMode::Jump => {
                if n < 0 {
                    // Jump to oldest entry.
                    if self.entries.is_empty() {
                        return None;
                    }
                    self.recall_pos = self.entries.len();
                } else {
                    // Jump to newest entry (or back to live line if already there).
                    if self.recall_pos == 0 {
                        return None;
                    }
                    self.recall_pos = 1;
                }
            }
            RecallMode::Exact => {
                let new_pos = self.recall_pos as i64 + n as i64;
                if new_pos < 0 {
                    // Moving forward past the live line.
                    if self.recall_pos == 0 {
                        return None;
                    }
                    self.recall_pos = 0;
                    return Some(&self.saved_line);
                }
                let new_pos = new_pos as usize;
                if new_pos > self.entries.len() {
                    return None; // already at oldest
                }
                self.recall_pos = new_pos;
            }
            RecallMode::Prefix => {
                let prefix = self.saved_line.clone();
                let start = self.recall_pos;
                let steps = n.unsigned_abs() as usize;

                if n > 0 {
                    // Search older.
                    let mut found = 0usize;
                    let mut pos = start;
                    for i in start..self.entries.len() {
                        if self.entries[i].starts_with(&prefix) {
                            found += 1;
                            if found == steps {
                                pos = i + 1;
                                break;
                            }
                        }
                    }
                    if pos == start {
                        return None; // no more matches
                    }
                    self.recall_pos = pos;
                } else {
                    // Search newer.
                    if start == 0 {
                        return None;
                    }
                    let mut found = 0usize;
                    let mut pos = start;
                    for i in (0..start - 1).rev() {
                        if self.entries[i].starts_with(&prefix) {
                            found += 1;
                            if found == steps {
                                pos = i + 1;
                                break;
                            }
                        }
                    }
                    if pos == start {
                        // Fell back to live line.
                        self.recall_pos = 0;
                        return Some(&self.saved_line);
                    }
                    self.recall_pos = pos;
                }
            }
        }

        if self.recall_pos == 0 {
            Some(&self.saved_line)
        } else {
            self.entries.get(self.recall_pos - 1).map(String::as_str)
        }
    }

    /// Reset the recall position back to the live line without clearing history.
    pub fn reset_recall(&mut self) {
        self.recall_pos = 0;
        self.saved_line.clear();
    }

    // ── ^old^new substitution ─────────────────────────────────────────────────

    /// Perform `^old^new` substitution on the most recent history entry.
    ///
    /// Returns the substituted string, or `None` if the format is wrong or
    /// the `old` string is not found in the last entry.
    ///
    /// Mirrors `history_sub()` in `history.c`.
    ///
    /// Input format: `^old^new` (the leading `^` is usually stripped by the
    /// caller before passing here, so the expected format is `old^new`).
    pub fn history_sub(&self, spec: &str) -> Option<String> {
        // Accept both "^old^new" and "old^new".
        let spec = spec.strip_prefix('^').unwrap_or(spec);
        let (old, new) = spec.split_once('^')?;
        let last = self.entries.front()?;
        if !last.contains(old) {
            return None;
        }
        Some(last.replacen(old, new, 1))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(entries: &[&str]) -> InputHistory {
        let mut h = InputHistory::new(100);
        // Record oldest first so the last entry is the most recent.
        for &e in entries.iter() {
            h.record(e);
        }
        h
    }

    // ── record ────────────────────────────────────────────────────────────────

    #[test]
    fn record_adds_entry() {
        let mut h = InputHistory::new(10);
        h.record("hello");
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn record_collapses_consecutive_duplicates() {
        let mut h = InputHistory::new(10);
        h.record("hello");
        h.record("hello");
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn record_keeps_non_consecutive_duplicates() {
        let mut h = InputHistory::new(10);
        h.record("hello");
        h.record("world");
        h.record("hello");
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn record_trims_to_max_size() {
        let mut h = InputHistory::new(3);
        for i in 0..5 {
            h.record(&format!("line{i}"));
        }
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn record_ignores_empty_line() {
        let mut h = InputHistory::new(10);
        h.record("");
        assert_eq!(h.len(), 0);
    }

    // ── recall exact ──────────────────────────────────────────────────────────

    #[test]
    fn recall_one_step_back() {
        let mut h = filled(&["first", "second", "third"]); // third is newest
        let r = h.recall(1, RecallMode::Exact, "current");
        assert_eq!(r, Some("third"));
    }

    #[test]
    fn recall_two_steps_back() {
        let mut h = filled(&["first", "second", "third"]);
        h.recall(1, RecallMode::Exact, "current");
        let r = h.recall(1, RecallMode::Exact, "third");
        assert_eq!(r, Some("second"));
    }

    #[test]
    fn recall_forward_to_live() {
        let mut h = filled(&["first", "second"]);
        h.recall(1, RecallMode::Exact, "live");
        let r = h.recall(-1, RecallMode::Exact, "second");
        assert_eq!(r, Some("live"));
    }

    #[test]
    fn recall_past_beginning_returns_none() {
        let mut h = filled(&["only"]);
        h.recall(1, RecallMode::Exact, "current");
        let r = h.recall(1, RecallMode::Exact, "only");
        assert!(r.is_none());
    }

    #[test]
    fn recall_past_live_returns_none() {
        let mut h = filled(&["first"]);
        let r = h.recall(-1, RecallMode::Exact, "current");
        assert!(r.is_none());
    }

    // ── recall jump ───────────────────────────────────────────────────────────

    #[test]
    fn recall_jump_to_oldest() {
        let mut h = filled(&["first", "second", "third"]);
        let r = h.recall(-1, RecallMode::Jump, "live");
        assert_eq!(r, Some("first"));
    }

    #[test]
    fn recall_jump_to_newest() {
        let mut h = filled(&["first", "second", "third"]);
        h.recall(-1, RecallMode::Jump, "live"); // go to oldest
        let r = h.recall(1, RecallMode::Jump, "first");
        assert_eq!(r, Some("third"));
    }

    // ── recall prefix ─────────────────────────────────────────────────────────

    #[test]
    fn recall_prefix_search() {
        let mut h = filled(&["go north", "look", "go south", "go east"]);
        // Search backward for entries starting with "go"; "live" = "go"
        let r = h.recall(1, RecallMode::Prefix, "go");
        assert_eq!(r, Some("go east"));
    }

    #[test]
    fn recall_prefix_no_match_returns_none() {
        let mut h = filled(&["look", "inventory"]);
        let r = h.recall(1, RecallMode::Prefix, "zzz");
        assert!(r.is_none());
    }

    // ── history_sub ───────────────────────────────────────────────────────────

    #[test]
    fn history_sub_basic() {
        let mut h = InputHistory::new(10);
        h.record("go north");
        let r = h.history_sub("north^south");
        assert_eq!(r, Some("go south".to_owned()));
    }

    #[test]
    fn history_sub_with_caret_prefix() {
        let mut h = InputHistory::new(10);
        h.record("go north");
        let r = h.history_sub("^north^south");
        assert_eq!(r, Some("go south".to_owned()));
    }

    #[test]
    fn history_sub_not_found_returns_none() {
        let mut h = InputHistory::new(10);
        h.record("go north");
        let r = h.history_sub("east^west");
        assert!(r.is_none());
    }

    #[test]
    fn history_sub_empty_history_returns_none() {
        let h = InputHistory::new(10);
        let r = h.history_sub("old^new");
        assert!(r.is_none());
    }

    #[test]
    fn history_sub_replaces_first_occurrence() {
        let mut h = InputHistory::new(10);
        h.record("aaa");
        let r = h.history_sub("a^b");
        assert_eq!(r, Some("baa".to_owned())); // only first
    }
}
