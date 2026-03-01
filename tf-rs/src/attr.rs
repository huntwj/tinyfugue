//! Text display attributes.
//!
//! Corresponds to `attr_t` / `cattr_t` and the `enum_attr` flags in `tf.h`.
//! An [`Attr`] value encodes style bits (bold, italic, …) and optional
//! foreground/background color indices in a single `u32`.

use std::ops::{BitAnd, BitOr, BitOrAssign, Not};

/// Text display attributes for a character or line.
///
/// Style flags and color are packed into a `u32`, matching the C `attr_t`
/// layout.  Use the associated constants and [`Attr::with_fg`] /
/// [`Attr::with_bg`] to build values; use [`Attr::contains`],
/// [`Attr::fg_color`], and [`Attr::bg_color`] to inspect them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attr(u32);

impl Attr {
    // ── Style flags (low bits) ────────────────────────────────────────────
    pub const UNDERLINE: Self = Self(0x0001);
    pub const REVERSE: Self   = Self(0x0002);
    pub const BOLD: Self      = Self(0x0004);
    pub const ITALIC: Self    = Self(0x0008);
    /// Highlight: a user-visible emphasis that doesn't map to a specific style.
    pub const HILITE: Self    = Self(0x0010);
    /// Explicitly marks "no attributes" (distinct from the zero value [`EMPTY`]).
    ///
    /// # Sentinel semantics
    ///
    /// [`EMPTY`] (`Attr(0)`) is the natural zero value produced by `Default` or
    /// bitwise operations on no flags.  `NONE` is a *positive* marker — bit 5 set
    /// — used in C TF when an attribute field must explicitly communicate "no
    /// formatting" rather than "unspecified".
    ///
    /// Concretely: `attr.contains(Attr::NONE)` returns `false` for an `EMPTY`
    /// attr, because `NONE` has bit 5 set and `EMPTY` has no bits set.  Only
    /// values that were explicitly created with `NONE` pass `contains(NONE)`.
    ///
    /// Use [`EMPTY`] / [`is_empty`] for "nothing set"; use `NONE` only when you
    /// need to distinguish "explicitly no-attrs" from "unset" in the same field.
    ///
    /// [`EMPTY`]: Self::EMPTY
    /// [`is_empty`]: Self::is_empty
    pub const NONE: Self      = Self(0x0020);
    pub const EXCLUSIVE: Self = Self(0x0040);

    // ── Color encoding (16-color mode) ────────────────────────────────────
    // Foreground: flag at bit 7, 4-bit index at bits 8-11.
    const FG_FLAG: u32  = 0x0080;
    const FG_MASK: u32  = 0x0f00;
    const FG_SHIFT: u32 = 8;
    // Background: flag at bit 12, 3-bit index at bits 13-15.
    // NOTE: The C code placed BG_FLAG at 0x0100 (bit 8), which overlaps with
    // FG_MASK.  We use non-overlapping bits here for correctness.
    const BG_FLAG: u32  = 0x1000;
    const BG_MASK: u32  = 0xe000;
    const BG_SHIFT: u32 = 13;

    // ── Non-display flags (high bits) ─────────────────────────────────────
    pub const NO_ACTIVITY: Self = Self(0x0100_0000);
    pub const NO_LOG: Self      = Self(0x0200_0000);
    pub const BELL: Self        = Self(0x0400_0000);
    /// Gag (suppress display of) the line.
    pub const GAG: Self         = Self(0x0800_0000);
    pub const NO_HISTORY: Self  = Self(0x1000_0000);
    pub const TF_PROMPT: Self   = Self(0x2000_0000);
    pub const SERV_PROMPT: Self = Self(0x4000_0000);

    /// The empty/zero attribute value.
    pub const EMPTY: Self = Self(0);

    // ── Inspection ────────────────────────────────────────────────────────

    /// Returns `true` if no attribute bits are set.
    #[inline]
    pub fn is_empty(self) -> bool {
        self == Self::EMPTY
    }

    /// Serialize to a TF `-a` flag string (e.g. `"bug"` for bold+underline+gag).
    pub fn to_tf_flag(self) -> String {
        let mut s = String::new();
        if self.contains(Self::BOLD)      { s.push('b'); }
        if self.contains(Self::UNDERLINE) { s.push('u'); }
        if self.contains(Self::REVERSE)   { s.push('r'); }
        if self.contains(Self::ITALIC)    { s.push('i'); }
        if self.contains(Self::HILITE)    { s.push('h'); }
        if self.contains(Self::GAG)       { s.push('g'); }
        s
    }

    /// Returns `true` if all bits in `other` are set in `self`.
    #[inline]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the foreground color index, if one is set.
    #[inline]
    pub fn fg_color(self) -> Option<u8> {
        if self.0 & Self::FG_FLAG != 0 {
            Some(((self.0 & Self::FG_MASK) >> Self::FG_SHIFT) as u8)
        } else {
            None
        }
    }

    /// Returns the background color index, if one is set.
    #[inline]
    pub fn bg_color(self) -> Option<u8> {
        if self.0 & Self::BG_FLAG != 0 {
            Some(((self.0 & Self::BG_MASK) >> Self::BG_SHIFT) as u8)
        } else {
            None
        }
    }

    // ── Construction ──────────────────────────────────────────────────────

    /// Return a copy of `self` with the foreground color set to `color`.
    #[inline]
    pub fn with_fg(self, color: u8) -> Self {
        Self(
            (self.0 & !(Self::FG_FLAG | Self::FG_MASK))
                | Self::FG_FLAG
                | ((color as u32) << Self::FG_SHIFT),
        )
    }

    /// Return a copy of `self` with the background color set to `color`.
    #[inline]
    pub fn with_bg(self, color: u8) -> Self {
        Self(
            (self.0 & !(Self::BG_FLAG | Self::BG_MASK))
                | Self::BG_FLAG
                | ((color as u32) << Self::BG_SHIFT),
        )
    }

    /// Return a copy of `self` with the foreground color cleared.
    #[inline]
    pub fn without_fg(self) -> Self {
        Self(self.0 & !(Self::FG_FLAG | Self::FG_MASK))
    }

    /// Return a copy of `self` with the background color cleared.
    #[inline]
    pub fn without_bg(self) -> Self {
        Self(self.0 & !(Self::BG_FLAG | Self::BG_MASK))
    }
}

impl BitOr for Attr {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
}

impl BitOrAssign for Attr {
    fn bitor_assign(&mut self, rhs: Self) { self.0 |= rhs.0; }
}

impl BitAnd for Attr {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) }
}

impl Not for Attr {
    type Output = Self;
    fn not(self) -> Self { Self(!self.0) }
}

// ── Named colors (matching C's `enum_color` order) ────────────────────────

/// Standard 16-color palette indices, matching TF's `enum_color` order.
pub mod color {
    pub const BLACK: u8          = 0;
    pub const RED: u8            = 1;
    pub const GREEN: u8          = 2;
    pub const YELLOW: u8         = 3;
    pub const BLUE: u8           = 4;
    pub const MAGENTA: u8        = 5;
    pub const CYAN: u8           = 6;
    pub const WHITE: u8          = 7;
    pub const GRAY: u8           = 8;
    pub const BRIGHT_RED: u8     = 9;
    pub const BRIGHT_GREEN: u8   = 10;
    pub const BRIGHT_YELLOW: u8  = 11;
    pub const BRIGHT_BLUE: u8    = 12;
    pub const BRIGHT_MAGENTA: u8 = 13;
    pub const BRIGHT_CYAN: u8    = 14;
    pub const BRIGHT_WHITE: u8   = 15;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_flags_are_independent() {
        let a = Attr::BOLD | Attr::UNDERLINE;
        assert!(a.contains(Attr::BOLD));
        assert!(a.contains(Attr::UNDERLINE));
        assert!(!a.contains(Attr::ITALIC));
    }

    #[test]
    fn fg_color_roundtrip() {
        let a = Attr::BOLD.with_fg(color::RED);
        assert_eq!(a.fg_color(), Some(color::RED));
        assert!(a.contains(Attr::BOLD));
        assert_eq!(a.bg_color(), None);
    }

    #[test]
    fn bg_color_roundtrip() {
        let a = Attr::EMPTY.with_bg(color::BLUE);
        assert_eq!(a.bg_color(), Some(color::BLUE));
        assert_eq!(a.fg_color(), None);
    }

    #[test]
    fn fg_color_replace() {
        let a = Attr::EMPTY.with_fg(color::RED).with_fg(color::GREEN);
        assert_eq!(a.fg_color(), Some(color::GREEN));
    }

    #[test]
    fn without_fg_clears_color() {
        let a = Attr::BOLD.with_fg(color::CYAN).without_fg();
        assert_eq!(a.fg_color(), None);
        assert!(a.contains(Attr::BOLD));
    }

    #[test]
    fn high_bit_flags_do_not_corrupt_color() {
        let a = Attr::GAG | Attr::EMPTY.with_fg(color::WHITE);
        assert!(a.contains(Attr::GAG));
        assert_eq!(a.fg_color(), Some(color::WHITE));
    }

    #[test]
    fn default_is_empty() {
        assert_eq!(Attr::default(), Attr::EMPTY);
    }
}
