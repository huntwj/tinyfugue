//! Terminal rendering — crossterm-backed output, ANSI attribute mapping,
//! status line, and more-prompt.
//!
//! Corresponds to the rendering layer of `output.c` / `tty.c` in the C source.
//!
//! ## Architecture
//!
//! [`Terminal`] owns the raw terminal handle and knows how to:
//!
//! * Convert a TF [`Attr`] value to a [`crossterm`] content style.
//! * Render a [`PhysLine`] (from [`Screen`]) to the terminal, applying
//!   per-character and whole-line attributes.
//! * Draw the status bar at the bottom of the screen.
//! * Show / clear the `--More--` prompt.
//! * Handle terminal resize.
//!
//! Heavy integration (reading the screen in a loop, interleaving with
//! keyboard input) belongs to the Phase 9 event loop; this module only
//! provides the low-level primitives.

use std::io::{self, Write};

use crossterm::{
    cursor, queue,
    style::{Attribute, Attributes, Color, ContentStyle, Print, ResetColor, SetStyle, Stylize},
    terminal::{self, ClearType},
};

use crate::attr::Attr;
use crate::screen::{LogicalLine, PhysLine, Screen};

// ── Attr → crossterm style ────────────────────────────────────────────────────

/// Map a TF [`Attr`] value to a crossterm [`ContentStyle`].
///
/// The gag flag is intentionally not mapped here — callers should skip lines
/// marked gag before calling [`Terminal::render_line`].
pub fn attr_style(attr: Attr) -> ContentStyle {
    let mut style = ContentStyle::new();
    let mut attributes = Attributes::default();

    if attr.contains(Attr::BOLD) {
        attributes.set(Attribute::Bold);
    }
    if attr.contains(Attr::UNDERLINE) {
        attributes.set(Attribute::Underlined);
    }
    if attr.contains(Attr::ITALIC) {
        attributes.set(Attribute::Italic);
    }
    if attr.contains(Attr::REVERSE) {
        attributes.set(Attribute::Reverse);
    }

    style.attributes = attributes;

    if let Some(fg) = attr.fg_color() {
        style.foreground_color = Some(ansi_color(fg));
    }
    if let Some(bg) = attr.bg_color() {
        style.background_color = Some(ansi_color(bg));
    }

    style
}

/// Convert a TF 16-color index to a crossterm [`Color`].
fn ansi_color(idx: u8) -> Color {
    match idx {
        0 => Color::Black,
        1 => Color::DarkRed,
        2 => Color::DarkGreen,
        3 => Color::DarkYellow,
        4 => Color::DarkBlue,
        5 => Color::DarkMagenta,
        6 => Color::DarkCyan,
        7 => Color::Grey,
        8 => Color::DarkGrey,
        9 => Color::Red,
        10 => Color::Green,
        11 => Color::Yellow,
        12 => Color::Blue,
        13 => Color::Magenta,
        14 => Color::Cyan,
        15 => Color::White,
        _ => Color::Reset,
    }
}

// ── StatusLine ────────────────────────────────────────────────────────────────

/// The formatted content of the status bar.
///
/// In the C source the status bar is built by `regen_status_fields()` from a
/// list of `StatusField` records.  For Phase 6 we use a simpler model: each
/// [`StatusLine`] is a plain string pre-formatted by the caller, with an
/// optional attribute.
#[derive(Debug, Clone, Default)]
pub struct StatusLine {
    pub text: String,
    pub attr: Attr,
}

impl StatusLine {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attr: Attr::EMPTY,
        }
    }

    pub fn with_attr(mut self, attr: Attr) -> Self {
        self.attr = attr;
        self
    }
}

// ── Terminal ──────────────────────────────────────────────────────────────────

/// Wraps `stdout` with crossterm commands and tracks terminal geometry.
///
/// Call [`Terminal::enter_raw_mode`] once at startup and
/// [`Terminal::leave_raw_mode`] on exit (or use the RAII guard returned by
/// `enter_raw_mode`).
pub struct Terminal {
    /// Terminal width in columns.
    pub width: u16,
    /// Terminal height in rows.
    pub height: u16,
    /// Number of rows reserved for the status bar (≥ 1).
    pub status_height: u16,
    out: Box<dyn Write>,
}

impl Terminal {
    /// Create a [`Terminal`] writing to the given writer.
    ///
    /// Queries the current terminal size; falls back to 80×24 if unavailable.
    pub fn new(out: impl Write + 'static) -> io::Result<Self> {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        Ok(Self {
            width,
            height,
            status_height: 1,
            out: Box::new(out),
        })
    }

    /// Enable raw mode.  Returns a guard that disables it on drop.
    pub fn enter_raw_mode() -> io::Result<RawModeGuard> {
        terminal::enable_raw_mode()?;
        Ok(RawModeGuard(()))
    }

    /// Update stored dimensions after a `SIGWINCH` / resize event.
    pub fn handle_resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    /// The row index (0-based) of the input line (last row).
    pub fn input_row(&self) -> u16 {
        self.height.saturating_sub(1)
    }

    /// The row index (0-based) of the first status-bar row (above input).
    pub fn status_top(&self) -> u16 {
        self.height.saturating_sub(1 + self.status_height)
    }

    /// The row index (0-based) of the output area bottom (exclusive).
    pub fn output_bottom(&self) -> u16 {
        self.status_top()
    }

    /// Render the input line at the bottom row.
    ///
    /// Draws `text` truncated to the terminal width, then positions the cursor
    /// at `cursor_col` within the input row.
    pub fn render_input(&mut self, text: &str, cursor_col: usize) -> io::Result<()> {
        let row = self.input_row();
        let width = self.width as usize;
        // Collect chars for width-safe slicing.
        let chars: Vec<char> = text.chars().collect();
        // If the text is wider than the terminal, show the window ending at
        // cursor position so the cursor stays visible.
        let (display_start, cursor_x) = if cursor_col < width {
            (0, cursor_col as u16)
        } else {
            let start = cursor_col + 1 - width;
            (start, (width - 1) as u16)
        };
        let display: String = chars
            .get(display_start..)
            .unwrap_or(&[])
            .iter()
            .take(width)
            .collect();
        queue!(
            self.out,
            cursor::Hide,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::UntilNewLine),
            Print(&display),
            cursor::MoveTo(cursor_x, row),
            cursor::Show
        )
    }

    // ── Low-level rendering ───────────────────────────────────────────────────

    /// Flush the internal output buffer to the terminal.
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Write `text` followed by `\r\n` and flush.
    ///
    /// Suitable for use in raw mode where `\r\n` is required instead of
    /// bare `\n`.  Errors are silently discarded (terminal writes are
    /// best-effort).
    pub fn print_line(&mut self, text: &str) {
        let _ = write!(self.out, "{}\r\n", text);
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        queue!(
            self.out,
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )
    }

    /// Move the cursor to `(col, row)` (both 0-based).
    pub fn move_to(&mut self, col: u16, row: u16) -> io::Result<()> {
        queue!(self.out, cursor::MoveTo(col, row))
    }

    /// Write a styled string at the current cursor position.
    pub fn write_styled(&mut self, text: &str, style: ContentStyle) -> io::Result<()> {
        queue!(self.out, SetStyle(style), Print(text), ResetColor)
    }

    /// Erase from the cursor to the end of the current line.
    pub fn clear_to_eol(&mut self) -> io::Result<()> {
        queue!(self.out, terminal::Clear(ClearType::UntilNewLine))
    }

    // ── Line rendering ────────────────────────────────────────────────────────

    /// Render a single [`PhysLine`] from `screen` at terminal row `row`.
    ///
    /// Skips the line if it belongs to a gagged logical line.
    /// Applies the whole-line [`Attr`] to the entire row, then overlays
    /// per-character attributes where the [`TfStr`] has them.
    pub fn render_phys_line(
        &mut self,
        _screen: &Screen,
        pl: &PhysLine,
        ll: &LogicalLine,
        row: u16,
    ) -> io::Result<()> {
        if ll.attr.contains(Attr::GAG) {
            return Ok(());
        }

        queue!(self.out, cursor::MoveTo(0, row))?;

        // Indent for wrapped continuation lines.
        if pl.indent > 0 {
            queue!(self.out, Print(" ".repeat(pl.indent)))?;
        }

        let chars: Vec<char> = ll.content.data.chars().collect();
        let slice = &chars[pl.start..(pl.start + pl.len).min(chars.len())];

        let char_attrs = ll.content.char_attrs();

        let line_style = attr_style(ll.attr);

        if let Some(ca) = char_attrs {
            // Per-character attributes: emit runs of the same style.
            let base = pl.start;
            let mut run_start = 0;
            while run_start < slice.len() {
                let run_attr = ca.get(base + run_start).copied().unwrap_or(Attr::EMPTY);
                let run_end = slice[run_start..]
                    .iter()
                    .enumerate()
                    .take_while(|(i, _)| {
                        ca.get(base + run_start + i).copied().unwrap_or(Attr::EMPTY)
                            == run_attr
                    })
                    .count();
                let text: String = slice[run_start..run_start + run_end].iter().collect();
                // Merge line-level style with per-char style.
                let merged = merge_styles(line_style, attr_style(run_attr));
                queue!(self.out, SetStyle(merged), Print(&text), ResetColor)?;
                run_start += run_end;
            }
        } else {
            // Uniform line-level attributes.
            let text: String = slice.iter().collect();
            queue!(self.out, SetStyle(line_style), Print(&text), ResetColor)?;
        }

        self.clear_to_eol()
    }

    /// Render all visible lines of `screen` into the output area.
    pub fn render_screen(&mut self, screen: &Screen) -> io::Result<()> {
        let output_rows = self.output_bottom();
        let visible: Vec<_> = screen.visible_lines().collect();
        let start_row = output_rows.saturating_sub(visible.len() as u16);

        // Clear rows above the first line.
        for row in 0..start_row {
            queue!(
                self.out,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::UntilNewLine)
            )?;
        }

        for (i, (ll, pl)) in visible.iter().enumerate() {
            let row = start_row + i as u16;
            if row >= output_rows {
                break;
            }
            self.render_phys_line(screen, pl, ll, row)?;
        }

        Ok(())
    }

    // ── Status bar ────────────────────────────────────────────────────────────

    /// Draw the status bar at the bottom of the screen.
    ///
    /// `lines` must have at most `status_height` entries; extra entries are
    /// ignored.  Each entry is padded / truncated to the terminal width.
    pub fn render_status(&mut self, lines: &[StatusLine]) -> io::Result<()> {
        let width = self.width as usize;
        let top = self.status_top();

        for (i, sl) in lines.iter().enumerate().take(self.status_height as usize) {
            let row = top + i as u16;
            let text = pad_or_truncate(&sl.text, width);
            let style = attr_style(sl.attr | Attr::REVERSE); // default: reversed
            queue!(
                self.out,
                cursor::MoveTo(0, row),
                SetStyle(style),
                Print(&text),
                ResetColor
            )?;
        }

        // Clear any unused status rows.
        for i in lines.len()..self.status_height as usize {
            let row = top + i as u16;
            let blank = " ".repeat(width);
            let style = attr_style(Attr::REVERSE);
            queue!(
                self.out,
                cursor::MoveTo(0, row),
                SetStyle(style),
                Print(&blank),
                ResetColor
            )?;
        }

        Ok(())
    }

    // ── More prompt ───────────────────────────────────────────────────────────

    /// Display the `--More--` prompt at the bottom of the output area.
    pub fn show_more_prompt(&mut self) -> io::Result<()> {
        let row = self.output_bottom().saturating_sub(1);
        let style = ContentStyle::new().bold().reverse();
        queue!(
            self.out,
            cursor::MoveTo(0, row),
            SetStyle(style),
            Print("--More--"),
            ResetColor
        )?;
        self.flush()
    }

    /// Erase the `--More--` prompt.
    pub fn clear_more_prompt(&mut self) -> io::Result<()> {
        let row = self.output_bottom().saturating_sub(1);
        queue!(
            self.out,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::UntilNewLine)
        )?;
        self.flush()
    }

    // ── Bell ──────────────────────────────────────────────────────────────────

    /// Ring the terminal bell.
    pub fn bell(&mut self) -> io::Result<()> {
        queue!(self.out, Print('\x07'))
    }
}

// ── RawModeGuard ──────────────────────────────────────────────────────────────

/// RAII guard: disables raw mode when dropped.
pub struct RawModeGuard(());

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Move cursor to a known position and show it before leaving raw mode.
        let _ = crossterm::execute!(
            std::io::stdout(),
            cursor::Show,
            cursor::MoveTo(0, 0)
        );
        let _ = terminal::disable_raw_mode();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Merge a base style with a per-character overlay.  The overlay wins for
/// any attributes it explicitly sets; the base fills in the rest.
fn merge_styles(base: ContentStyle, overlay: ContentStyle) -> ContentStyle {
    ContentStyle {
        foreground_color: overlay.foreground_color.or(base.foreground_color),
        background_color: overlay.background_color.or(base.background_color),
        underline_color: overlay.underline_color.or(base.underline_color),
        attributes: base.attributes | overlay.attributes,
    }
}

/// Pad `s` with spaces to exactly `width` chars, or truncate if too long.
fn pad_or_truncate(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count >= width {
        s.chars().take(width).collect()
    } else {
        let mut out = s.to_owned();
        for _ in 0..(width - count) {
            out.push(' ');
        }
        out
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::color;

    // ── attr_style ────────────────────────────────────────────────────────────

    #[test]
    fn bold_maps_to_bold_attribute() {
        let s = attr_style(Attr::BOLD);
        assert!(s.attributes.has(Attribute::Bold));
        assert!(!s.attributes.has(Attribute::Italic));
    }

    #[test]
    fn underline_maps() {
        let s = attr_style(Attr::UNDERLINE);
        assert!(s.attributes.has(Attribute::Underlined));
    }

    #[test]
    fn italic_maps() {
        let s = attr_style(Attr::ITALIC);
        assert!(s.attributes.has(Attribute::Italic));
    }

    #[test]
    fn reverse_maps() {
        let s = attr_style(Attr::REVERSE);
        assert!(s.attributes.has(Attribute::Reverse));
    }

    #[test]
    fn fg_color_maps() {
        let a = Attr::EMPTY.with_fg(color::RED);
        let s = attr_style(a);
        assert_eq!(s.foreground_color, Some(Color::DarkRed));
    }

    #[test]
    fn bg_color_maps() {
        let a = Attr::EMPTY.with_bg(color::BLUE);
        let s = attr_style(a);
        assert_eq!(s.background_color, Some(Color::DarkBlue));
    }

    #[test]
    fn empty_attr_no_colors_no_attrs() {
        let s = attr_style(Attr::EMPTY);
        assert_eq!(s.foreground_color, None);
        assert_eq!(s.background_color, None);
        assert!(!s.attributes.has(Attribute::Bold));
    }

    #[test]
    fn bright_colors_map() {
        let a = Attr::EMPTY.with_fg(color::BRIGHT_RED);
        let s = attr_style(a);
        assert_eq!(s.foreground_color, Some(Color::Red));
    }

    // ── merge_styles ──────────────────────────────────────────────────────────

    #[test]
    fn overlay_fg_overrides_base() {
        let base = attr_style(Attr::EMPTY.with_fg(color::RED));
        let overlay = attr_style(Attr::EMPTY.with_fg(color::GREEN));
        let merged = merge_styles(base, overlay);
        assert_eq!(merged.foreground_color, Some(Color::DarkGreen));
    }

    #[test]
    fn overlay_no_fg_inherits_base() {
        let base = attr_style(Attr::EMPTY.with_fg(color::RED));
        let overlay = attr_style(Attr::BOLD);
        let merged = merge_styles(base, overlay);
        assert_eq!(merged.foreground_color, Some(Color::DarkRed));
        assert!(merged.attributes.has(Attribute::Bold));
    }

    // ── pad_or_truncate ───────────────────────────────────────────────────────

    #[test]
    fn pad_short_string() {
        assert_eq!(pad_or_truncate("hi", 5), "hi   ");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(pad_or_truncate("hello world", 5), "hello");
    }

    #[test]
    fn exact_length_unchanged() {
        assert_eq!(pad_or_truncate("exact", 5), "exact");
    }

    // ── StatusLine ────────────────────────────────────────────────────────────

    #[test]
    fn status_line_default_empty() {
        let sl = StatusLine::default();
        assert!(sl.text.is_empty());
        assert_eq!(sl.attr, Attr::EMPTY);
    }

    #[test]
    fn status_line_with_attr() {
        let sl = StatusLine::new("[ Avalon ]").with_attr(Attr::BOLD);
        assert_eq!(sl.text, "[ Avalon ]");
        assert!(sl.attr.contains(Attr::BOLD));
    }
}
