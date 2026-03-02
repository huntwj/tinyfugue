//! Output screen model: logical lines, word-wrapping, scrollback, and pagination.
//!
//! Corresponds to the `Screen` / `PhysLine` data structures in `tfio.h` and
//! the line-management routines in `output.c`.
//!
//! ## Terminology (matching the C source)
//!
//! * **Logical line** — one "paragraph" of output, exactly as received from
//!   the server or produced by a command.  Stored as a [`TfStr`] so that
//!   per-character display attributes are preserved.
//!
//! * **Physical line** — one row on the terminal.  A long logical line is
//!   split into multiple physical lines by the wrapping algorithm.
//!   Represented by [`PhysLine`], a lightweight reference into the logical
//!   line's character array.
//!
//! * **Scrollback** — how many physical lines the view has scrolled above
//!   the most-recent output.

use std::collections::VecDeque;

use crate::attr::Attr;
use crate::tfstr::TfStr;

// ── LogicalLine ───────────────────────────────────────────────────────────────

/// One paragraph of output text with per-character display attributes.
///
/// Corresponds to the `conString *` stored in each `PhysLine::str` in the C
/// source (multiple `PhysLine`s can reference the same `conString`).
#[derive(Debug, Clone)]
pub struct LogicalLine {
    /// The text and its per-character attributes.
    pub content: TfStr,
    /// Whole-line attribute flags (gag, highlight colour, etc.).
    pub attr: Attr,
}

impl LogicalLine {
    pub fn new(content: TfStr, attr: Attr) -> Self {
        Self { content, attr }
    }

    /// Construct from a plain string with no attributes.
    pub fn plain(text: &str) -> Self {
        let mut t = TfStr::new();
        t.push_str(text, None);
        Self {
            content: t,
            attr: Attr::EMPTY,
        }
    }
}

// ── PhysLine ─────────────────────────────────────────────────────────────────

/// A single terminal row, representing a slice of a [`LogicalLine`].
///
/// Corresponds to `struct PhysLine` in `tfio.h`.
#[derive(Debug, Clone)]
pub struct PhysLine {
    /// Index into [`Screen::lines`] of the parent logical line.
    pub logical_idx: usize,
    /// Character offset within the logical line where this row starts.
    pub start: usize,
    /// Number of characters in this row.
    pub len: usize,
    /// Leading spaces added for word-wrap continuation indent.
    pub indent: usize,
}

// ── Screen ────────────────────────────────────────────────────────────────────

/// Scrollback buffer and pagination state for one output window.
///
/// Corresponds to `struct Screen` in `tfio.h`.
///
/// The screen holds a bounded ring of [`LogicalLine`]s and a flat list of
/// [`PhysLine`]s derived by wrapping.  The view is defined by [`Screen::bot`]
/// (the bottom-most physical line in the view) and [`Screen::scrollback`]
/// (how many physical lines above `bot` the view is scrolled).
#[derive(Debug)]
pub struct Screen {
    // ── Configuration ─────────────────────────────────────────────────────
    /// Terminal column width used for wrapping.
    pub wrap_width: usize,
    /// Maximum number of logical lines kept in the scrollback buffer.
    pub max_lines: usize,
    /// How many physical lines to display at once (output window height).
    pub view_height: usize,
    /// Lines of output before a `--More--` pause; 0 means no pausing.
    pub more_threshold: usize,

    // ── Content ───────────────────────────────────────────────────────────
    /// Logical lines, oldest first.
    lines: VecDeque<LogicalLine>,
    /// Physical lines derived from `lines`, oldest first.
    physlines: Vec<PhysLine>,

    // ── View state ────────────────────────────────────────────────────────
    /// Number of physical lines scrolled above the bottom of the buffer.
    /// 0 = showing the most-recent output.
    scrollback: usize,
    /// Lines of new output displayed since the last `--More--` check.
    outcount: usize,
    /// Whether the screen is currently paused at a `--More--` prompt.
    pub paused: bool,
}

impl Screen {
    /// Create a screen with the given terminal dimensions and defaults.
    pub fn new(wrap_width: usize, view_height: usize) -> Self {
        Self {
            wrap_width,
            max_lines: 1000,
            view_height,
            more_threshold: 0, // disabled by default
            lines: VecDeque::new(),
            physlines: Vec::new(),
            scrollback: 0,
            outcount: 0,
            paused: false,
        }
    }

    // ── Adding output ─────────────────────────────────────────────────────

    /// Append a logical line to the screen, wrapping it into physical lines.
    ///
    /// Returns `true` if the `--More--` threshold has been reached and the
    /// caller should display a more-prompt.
    pub fn push_line(&mut self, line: LogicalLine) -> bool {
        let char_count = line.content.char_count();
        let logical_idx = self.lines.len();
        let phys_before = self.physlines.len();

        // Wrap the logical line into physical lines.
        if char_count == 0 {
            // Empty line still occupies one physical row.
            self.physlines.push(PhysLine {
                logical_idx,
                start: 0,
                len: 0,
                indent: 0,
            });
        } else {
            let mut start = 0;
            let mut first = true;
            while start < char_count {
                let available = if first {
                    self.wrap_width
                } else {
                    self.wrap_width.saturating_sub(WRAP_INDENT)
                };
                let len = available.min(char_count - start);
                self.physlines.push(PhysLine {
                    logical_idx,
                    start,
                    len,
                    indent: if first { 0 } else { WRAP_INDENT },
                });
                start += len;
                first = false;
            }
        }

        // phys lines added for this logical line (physlines were appended above)
        let added_phys = self.physlines.len() - phys_before;
        self.lines.push_back(line);

        // If the user is scrolled back, advance scrollback by the number of
        // physical lines added so the viewport stays anchored at the same
        // position in the output history.
        if self.scrollback > 0 {
            self.scrollback += added_phys;
        }

        let pre_trim = self.physlines.len();
        self.trim_to_max();
        // If trim removed lines from the top, reduce scrollback accordingly.
        let trimmed = pre_trim - self.physlines.len();
        self.scrollback = self.scrollback.saturating_sub(trimmed);

        // Pagination check.
        if self.more_threshold > 0 && !self.paused {
            self.outcount += 1;
            if self.outcount >= self.more_threshold {
                self.paused = true;
                self.outcount = 0;
                return true;
            }
        }
        false
    }

    /// Dismiss the `--More--` prompt and resume output.
    pub fn unpause(&mut self) {
        self.paused = false;
        self.outcount = 0;
    }

    // ── Scrolling ─────────────────────────────────────────────────────────

    /// Scroll up by `n` physical lines (towards older output).
    /// Returns the actual number of lines scrolled.
    pub fn scroll_up(&mut self, n: usize) -> usize {
        let max_scroll = self.physlines.len().saturating_sub(self.view_height);
        let delta = n.min(max_scroll - self.scrollback);
        self.scrollback += delta;
        delta
    }

    /// Scroll down by `n` physical lines (towards newer output).
    /// Returns the actual number of lines scrolled.
    pub fn scroll_down(&mut self, n: usize) -> usize {
        let delta = n.min(self.scrollback);
        self.scrollback -= delta;
        delta
    }

    /// Scroll to the most-recent output (bottom).
    pub fn scroll_to_bottom(&mut self) {
        self.scrollback = 0;
    }

    /// Scroll to the oldest output (top of scrollback buffer).
    pub fn scroll_to_top(&mut self) {
        let max_scroll = self.physlines.len().saturating_sub(self.view_height);
        self.scrollback = max_scroll;
    }

    /// How many physical lines are scrolled above the bottom.
    pub fn scrollback(&self) -> usize {
        self.scrollback
    }

    /// Iterate all logical lines in chronological order (oldest first).
    pub fn iter_lines(&self) -> impl DoubleEndedIterator<Item = &LogicalLine> {
        self.lines.iter()
    }

    // ── View access ───────────────────────────────────────────────────────

    /// Return the physical lines currently visible in the output window,
    /// bottom-most last.
    ///
    /// Returns fewer than `view_height` lines if the buffer is not full yet.
    pub fn visible_lines(&self) -> impl Iterator<Item = (&LogicalLine, &PhysLine)> {
        let total = self.physlines.len();
        let bot = total.saturating_sub(self.scrollback);
        let top = bot.saturating_sub(self.view_height);
        self.physlines[top..bot]
            .iter()
            .map(|pl| (&self.lines[pl.logical_idx], pl))
    }

    /// Total number of logical lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Total number of physical lines in the buffer.
    pub fn phys_count(&self) -> usize {
        self.physlines.len()
    }

    // ── Resize ────────────────────────────────────────────────────────────

    /// Adapt to a new terminal width/height.  Re-wraps all lines.
    pub fn resize(&mut self, new_width: usize, new_height: usize) {
        self.wrap_width = new_width;
        self.view_height = new_height;
        self.rewrap();
    }

    // ── Internal ─────────────────────────────────────────────────────────

    /// Drop oldest logical lines when the buffer exceeds `max_lines`.
    ///
    /// O(n) in `physlines` regardless of how many logical lines are dropped:
    /// one linear scan to find the split point, one `drain`, one linear pass
    /// to decrement indices.
    fn trim_to_max(&mut self) {
        let drop_count = self.lines.len().saturating_sub(self.max_lines);
        if drop_count == 0 {
            return;
        }
        for _ in 0..drop_count {
            self.lines.pop_front();
        }
        // Find the first physline belonging to a surviving logical line.
        // Physlines are in order, so `logical_idx >= drop_count` marks the boundary.
        let split = self
            .physlines
            .partition_point(|pl| pl.logical_idx < drop_count);
        self.physlines.drain(..split);
        // Re-index surviving physlines in one pass.
        for pl in &mut self.physlines {
            pl.logical_idx -= drop_count;
        }
    }

    /// Rebuild the physlines list after a wrap-width change.
    fn rewrap(&mut self) {
        self.physlines.clear();
        let lines: Vec<_> = self.lines.iter().cloned().collect();
        let scrollback_save = self.scrollback;
        self.scrollback = 0;
        self.outcount = 0;
        for (idx, line) in lines.into_iter().enumerate() {
            let char_count = line.content.char_count();
            if char_count == 0 {
                self.physlines.push(PhysLine {
                    logical_idx: idx,
                    start: 0,
                    len: 0,
                    indent: 0,
                });
            } else {
                let mut start = 0;
                let mut first = true;
                while start < char_count {
                    let available = if first {
                        self.wrap_width
                    } else {
                        self.wrap_width.saturating_sub(WRAP_INDENT)
                    };
                    let len = available.min(char_count - start);
                    self.physlines.push(PhysLine {
                        logical_idx: idx,
                        start,
                        len,
                        indent: if first { 0 } else { WRAP_INDENT },
                    });
                    start += len;
                    first = false;
                }
            }
        }
        // Restore scrollback, clamped to new bounds.
        let max_scroll = self.physlines.len().saturating_sub(self.view_height);
        self.scrollback = scrollback_save.min(max_scroll);
    }
}

/// Spaces added at the start of continuation lines (matches C `wrapspace`).
const WRAP_INDENT: usize = 0;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_line(text: &str) -> LogicalLine {
        LogicalLine::plain(text)
    }

    // ── LogicalLine ───────────────────────────────────────────────────────────

    #[test]
    fn logical_line_plain() {
        let ll = LogicalLine::plain("hello");
        assert_eq!(ll.content.char_count(), 5);
        assert_eq!(ll.attr, Attr::EMPTY);
    }

    // ── Screen basics ─────────────────────────────────────────────────────────

    #[test]
    fn push_single_short_line() {
        let mut s = Screen::new(80, 24);
        s.push_line(plain_line("hello"));
        assert_eq!(s.line_count(), 1);
        assert_eq!(s.phys_count(), 1);
    }

    #[test]
    fn empty_line_counts_as_one_physline() {
        let mut s = Screen::new(80, 24);
        s.push_line(plain_line(""));
        assert_eq!(s.phys_count(), 1);
    }

    #[test]
    fn long_line_wraps() {
        let mut s = Screen::new(10, 24);
        // 25 chars → ceil(25/10) = 3 physical lines
        s.push_line(plain_line("abcdefghijklmnopqrstuvwxy"));
        assert_eq!(s.phys_count(), 3);
    }

    #[test]
    fn exact_width_line_no_wrap() {
        let mut s = Screen::new(10, 24);
        s.push_line(plain_line("0123456789")); // exactly 10
        assert_eq!(s.phys_count(), 1);
    }

    // ── Scrollback ────────────────────────────────────────────────────────────

    #[test]
    fn scroll_up_and_down() {
        let mut s = Screen::new(80, 5);
        for i in 0..10 {
            s.push_line(plain_line(&format!("line {i}")));
        }
        assert_eq!(s.scrollback(), 0);
        let moved = s.scroll_up(3);
        assert_eq!(moved, 3);
        assert_eq!(s.scrollback(), 3);
        let moved = s.scroll_down(2);
        assert_eq!(moved, 2);
        assert_eq!(s.scrollback(), 1);
    }

    #[test]
    fn scrollback_anchors_when_new_lines_arrive() {
        // When the user has scrolled up, new lines should NOT move the viewport.
        let mut s = Screen::new(80, 3);
        for i in 0..6 {
            s.push_line(plain_line(&format!("line {i}")));
        }
        // Scroll up 2 physical lines.
        s.scroll_up(2);
        assert_eq!(s.scrollback(), 2);
        let visible_before: Vec<_> = s.visible_lines()
            .map(|(ll, _)| ll.content.data.clone())
            .collect();
        // Add a new line.
        s.push_line(plain_line("new line"));
        // Scrollback should have advanced by 1 to stay anchored.
        assert_eq!(s.scrollback(), 3);
        // Visible content should be the same as before.
        let visible_after: Vec<_> = s.visible_lines()
            .map(|(ll, _)| ll.content.data.clone())
            .collect();
        assert_eq!(visible_before, visible_after);
    }

    #[test]
    fn scroll_up_clamped_to_buffer() {
        let mut s = Screen::new(80, 5);
        for i in 0..4 {
            s.push_line(plain_line(&format!("{i}")));
        }
        // Only 4 lines, view_height=5 → no headroom to scroll.
        assert_eq!(s.scroll_up(100), 0);
    }

    #[test]
    fn scroll_to_top_and_bottom() {
        let mut s = Screen::new(80, 3);
        for i in 0..10 {
            s.push_line(plain_line(&format!("{i}")));
        }
        s.scroll_to_top();
        assert!(s.scrollback() > 0);
        s.scroll_to_bottom();
        assert_eq!(s.scrollback(), 0);
    }

    // ── Visible lines ─────────────────────────────────────────────────────────

    #[test]
    fn visible_lines_returns_at_most_view_height() {
        let mut s = Screen::new(80, 5);
        for i in 0..20 {
            s.push_line(plain_line(&format!("{i}")));
        }
        assert_eq!(s.visible_lines().count(), 5);
    }

    #[test]
    fn visible_lines_fewer_than_view_height_when_buffer_small() {
        let mut s = Screen::new(80, 24);
        s.push_line(plain_line("only one"));
        assert_eq!(s.visible_lines().count(), 1);
    }

    // ── More / pagination ─────────────────────────────────────────────────────

    #[test]
    fn more_threshold_triggers_pause() {
        let mut s = Screen::new(80, 24);
        s.more_threshold = 3;
        let r1 = s.push_line(plain_line("1"));
        let r2 = s.push_line(plain_line("2"));
        let r3 = s.push_line(plain_line("3"));
        assert!(!r1);
        assert!(!r2);
        assert!(r3, "third line should trigger more-pause");
        assert!(s.paused);
    }

    #[test]
    fn unpause_resets_state() {
        let mut s = Screen::new(80, 24);
        s.more_threshold = 2;
        s.push_line(plain_line("a"));
        s.push_line(plain_line("b")); // triggers pause
        assert!(s.paused);
        s.unpause();
        assert!(!s.paused);
        // Next two lines should not re-pause immediately.
        assert!(!s.push_line(plain_line("c")));
    }

    // ── Resize ────────────────────────────────────────────────────────────────

    #[test]
    fn resize_rewraps() {
        let mut s = Screen::new(10, 24);
        // 15 chars → 2 physical lines at width 10
        s.push_line(plain_line("abcdefghijklmno"));
        assert_eq!(s.phys_count(), 2);
        // Widen to 20 → fits in 1 physical line
        s.resize(20, 24);
        assert_eq!(s.phys_count(), 1);
    }

    // ── Max lines ─────────────────────────────────────────────────────────────

    #[test]
    fn max_lines_trims_oldest() {
        let mut s = Screen::new(80, 24);
        s.max_lines = 5;
        for i in 0..10 {
            s.push_line(plain_line(&format!("{i}")));
        }
        assert_eq!(s.line_count(), 5);
    }
}
