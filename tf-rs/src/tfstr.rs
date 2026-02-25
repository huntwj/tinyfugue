//! Owned string type with optional per-character display attributes.
//!
//! Corresponds to `String` / `conString` + the `charattrs` field in
//! `dstring.h`.  Unlike the C version, there is no reference counting or
//! manual capacity management — Rust's ownership system and [`std::string::String`]
//! handle those concerns.

use crate::attr::Attr;

/// An owned, growable string that may carry per-character display attributes.
///
/// `char_attrs`, when present, has exactly one [`Attr`] per Unicode scalar
/// value (i.e. per [`char`]) in `data`.  When absent, all characters share
/// the same attribute, determined by context (e.g. the line-level attribute
/// on the containing object).
#[derive(Debug, Clone, Default)]
pub struct TfStr {
    pub data: String,
    char_attrs: Option<Vec<Attr>>,
}

impl TfStr {
    /// Create a new, empty `TfStr`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Length in bytes (matching [`str::len`]).
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Number of Unicode characters (matching [`str::chars().count()`]).
    pub fn char_count(&self) -> usize {
        self.data.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns a reference to the per-character attribute slice, if present.
    pub fn char_attrs(&self) -> Option<&[Attr]> {
        self.char_attrs.as_deref()
    }

    /// Push a single character with an optional per-character attribute.
    ///
    /// If `attr` is [`Some`] and this is the first attributed character,
    /// the `char_attrs` vector is initialised and back-filled with
    /// [`Attr::EMPTY`] for all previously pushed characters.
    pub fn push(&mut self, ch: char, attr: Option<Attr>) {
        self.data.push(ch);
        match (attr, &mut self.char_attrs) {
            (Some(a), Some(v)) => v.push(a),
            (Some(a), slot @ None) => {
                let prior = self.data.chars().count() - 1;
                let mut v = vec![Attr::EMPTY; prior];
                v.push(a);
                *slot = Some(v);
            }
            (None, Some(v)) => v.push(Attr::EMPTY),
            (None, None) => {}
        }
    }

    /// Push a string slice, applying the same optional attribute to every character.
    pub fn push_str(&mut self, s: &str, attr: Option<Attr>) {
        for ch in s.chars() {
            self.push(ch, attr);
        }
    }

    /// Clear the string and its attribute vector, retaining allocations.
    pub fn clear(&mut self) {
        self.data.clear();
        if let Some(v) = &mut self.char_attrs {
            v.clear();
        }
    }

    /// Returns the attribute for the `n`th Unicode character, if attributes
    /// are present.
    ///
    /// # Panics
    /// Panics if `n >= self.char_count()`.
    pub fn attr_at(&self, n: usize) -> Option<Attr> {
        self.char_attrs.as_ref().map(|v| v[n])
    }

    /// Iterate over `(char, Attr)` pairs.
    ///
    /// If no per-character attributes are set, every character yields
    /// [`Attr::EMPTY`].
    pub fn chars_and_attrs(&self) -> impl Iterator<Item = (char, Attr)> + '_ {
        let attrs = self.char_attrs.as_deref();
        self.data
            .chars()
            .enumerate()
            .map(move |(i, ch)| (ch, attrs.map_or(Attr::EMPTY, |v| v[i])))
    }
}

impl TfStr {
    /// Parse a string that may contain TF `@{...}` attribute sequences,
    /// returning a [`TfStr`] with per-character display attributes.
    ///
    /// Recognized codes inside `@{...}`:
    /// - `n` or empty — reset all attributes to [`Attr::EMPTY`]
    /// - `b`/`B` bold · `u`/`U` underline · `i`/`I` italic
    /// - `r`/`R` reverse · `h`/`H` hilite · `f`/`F` flash (ignored) · `d`/`D` dim (ignored)
    /// - `C<name>` — set foreground color; `Cbg<name>` — set background color
    ///
    /// Named colors: `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
    /// `cyan`, `white`, and `bright`-prefixed variants.  RGB colors (`rgbXYZ`,
    /// X/Y/Z each 0–5) are mapped to the nearest 16-color value.  Unknown
    /// sequences are stripped silently.
    pub fn from_tf_markup(text: &str) -> Self {
        let mut out = TfStr::new();
        let mut cur = Attr::EMPTY;
        let mut chars = text.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '@' && chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut spec = String::new();
                loop {
                    match chars.next() {
                        None | Some('}') => break,
                        Some(c) => spec.push(c),
                    }
                }
                cur = tf_apply_spec(&spec, cur);
            } else {
                let attr = if cur == Attr::EMPTY { None } else { Some(cur) };
                out.push(ch, attr);
            }
        }
        out
    }
}

// ── @{...} attribute-spec helpers ─────────────────────────────────────────────

fn tf_apply_spec(spec: &str, cur: Attr) -> Attr {
    let spec = spec.trim();
    if spec.is_empty() || spec == "n" {
        return Attr::EMPTY;
    }
    let mut attr = cur;
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'n'       => { attr = Attr::EMPTY; }
            'b' | 'B' => { attr |= Attr::BOLD; }
            'u' | 'U' => { attr |= Attr::UNDERLINE; }
            'i' | 'I' => { attr |= Attr::ITALIC; }
            'r' | 'R' => { attr |= Attr::REVERSE; }
            'h' | 'H' => { attr |= Attr::HILITE; }
            'f' | 'F' | 'd' | 'D' => {} // flash / dim — no direct mapping
            'C' => {
                let name: String = chars.collect();
                if let Some(rest) = name.strip_prefix("bg") {
                    if let Some(idx) = tf_color_index(rest) {
                        attr = attr.with_bg(idx & 0x07); // 3-bit BG in current Attr layout
                    }
                } else if let Some(idx) = tf_color_index(&name) {
                    attr = attr.with_fg(idx);
                }
                break; // color consumed rest of spec
            }
            _ => {}
        }
    }
    attr
}

fn tf_color_index(name: &str) -> Option<u8> {
    match name.to_lowercase().as_str() {
        "black"                                      => Some(0),
        "red"                                        => Some(1),
        "green"                                      => Some(2),
        "yellow"                                     => Some(3),
        "blue"                                       => Some(4),
        "magenta"                                    => Some(5),
        "cyan"                                       => Some(6),
        "white"                                      => Some(7),
        "gray" | "grey" | "darkgray" | "brightblack" => Some(8),
        "brightred"                                  => Some(9),
        "brightgreen"                                => Some(10),
        "brightyellow"                               => Some(11),
        "brightblue"                                 => Some(12),
        "brightmagenta"                              => Some(13),
        "brightcyan"                                 => Some(14),
        "brightwhite"                                => Some(15),
        s if s.starts_with("rgb") && s.len() == 6 => {
            let mut digits = [0u8; 3];
            for (i, b) in s[3..].bytes().enumerate() {
                if i >= 3 || !(b'0'..=b'5').contains(&b) { return None; }
                digits[i] = b - b'0';
            }
            Some(tf_rgb_to_16(digits[0], digits[1], digits[2]))
        }
        _ => None,
    }
}

/// Map an `rgbXYZ` colour (X, Y, Z each 0–5) to the nearest 16-colour index.
fn tf_rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        return match r { 0 => 0, 1 | 2 => 8, 3 | 4 => 7, _ => 15 };
    }
    let bright = r >= 3 || g >= 3 || b >= 3;
    let base: u8 = match (r > 0, g > 0, b > 0) {
        (true,  true,  true)  => 7,
        (true,  true,  false) => 3,
        (true,  false, true)  => 5,
        (false, true,  true)  => 6,
        (true,  false, false) => 1,
        (false, true,  false) => 2,
        (false, false, true)  => 4,
        (false, false, false) => 0,
    };
    base + if bright { 8 } else { 0 }
}

impl From<&str> for TfStr {
    fn from(s: &str) -> Self {
        Self {
            data: s.to_owned(),
            char_attrs: None,
        }
    }
}

impl std::str::FromStr for TfStr {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

impl From<String> for TfStr {
    fn from(s: String) -> Self {
        Self {
            data: s,
            char_attrs: None,
        }
    }
}

impl std::fmt::Display for TfStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::color;

    #[test]
    fn plain_string_has_no_attrs() {
        let s = TfStr::from("hello");
        assert_eq!(s.len(), 5);
        assert_eq!(s.char_count(), 5);
        assert!(s.char_attrs().is_none());
    }

    #[test]
    fn push_without_attr_stays_plain() {
        let mut s = TfStr::new();
        s.push('a', None);
        s.push('b', None);
        assert!(s.char_attrs().is_none());
    }

    #[test]
    fn push_with_attr_initialises_vector() {
        let mut s = TfStr::new();
        s.push('a', None);
        s.push('b', Some(Attr::BOLD));
        // 'a' gets EMPTY, 'b' gets BOLD
        assert_eq!(s.attr_at(0), Some(Attr::EMPTY));
        assert_eq!(s.attr_at(1), Some(Attr::BOLD));
    }

    #[test]
    fn push_str_uniform_attr() {
        let mut s = TfStr::new();
        let bold_red = Attr::BOLD.with_fg(color::RED);
        s.push_str("hi", Some(bold_red));
        assert_eq!(s.char_count(), 2);
        assert_eq!(s.attr_at(0), Some(bold_red));
        assert_eq!(s.attr_at(1), Some(bold_red));
    }

    #[test]
    fn clear_resets_string_and_attrs() {
        let mut s = TfStr::new();
        s.push_str("hello", Some(Attr::ITALIC));
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.char_attrs().map(|v| v.len()), Some(0));
    }

    #[test]
    fn chars_and_attrs_plain() {
        let s = TfStr::from("ab");
        let pairs: Vec<_> = s.chars_and_attrs().collect();
        assert_eq!(pairs, vec![('a', Attr::EMPTY), ('b', Attr::EMPTY)]);
    }

    #[test]
    fn chars_and_attrs_mixed() {
        let mut s = TfStr::new();
        s.push('x', None);
        s.push('y', Some(Attr::UNDERLINE));
        let pairs: Vec<_> = s.chars_and_attrs().collect();
        assert_eq!(pairs[0], ('x', Attr::EMPTY));
        assert_eq!(pairs[1], ('y', Attr::UNDERLINE));
    }

    #[test]
    fn multibyte_char_count() {
        // '€' is 3 bytes in UTF-8
        let mut s = TfStr::new();
        s.push('€', Some(Attr::BOLD));
        s.push('!', Some(Attr::ITALIC));
        assert_eq!(s.len(), 4);       // bytes
        assert_eq!(s.char_count(), 2); // chars
        assert_eq!(s.attr_at(0), Some(Attr::BOLD));
        assert_eq!(s.attr_at(1), Some(Attr::ITALIC));
    }

    #[test]
    fn from_tf_markup_plain() {
        let s = TfStr::from_tf_markup("hello");
        assert_eq!(s.data, "hello");
        assert!(s.char_attrs().is_none());
    }

    #[test]
    fn from_tf_markup_bold_reset() {
        let s = TfStr::from_tf_markup("@{B}hi@{n}bye");
        assert_eq!(s.data, "hibye");
        let attrs = s.char_attrs().unwrap();
        assert_eq!(attrs[0], Attr::BOLD);
        assert_eq!(attrs[1], Attr::BOLD);
        assert_eq!(attrs[2], Attr::EMPTY);
        assert_eq!(attrs[3], Attr::EMPTY);
        assert_eq!(attrs[4], Attr::EMPTY);
    }

    #[test]
    fn from_tf_markup_fg_color() {
        let s = TfStr::from_tf_markup("@{Cred}x@{n}");
        assert_eq!(s.data, "x");
        let attrs = s.char_attrs().unwrap();
        assert_eq!(attrs[0].fg_color(), Some(color::RED));
    }

    #[test]
    fn from_tf_markup_bg_color() {
        let s = TfStr::from_tf_markup("@{Cbgblue}x@{n}");
        assert_eq!(s.data, "x");
        // BG is stored in 3-bit field, blue=4 → 4 & 0x07 = 4
        assert_eq!(attrs_for(&s)[0].bg_color(), Some(4));
    }

    #[test]
    fn from_tf_markup_rgb_red() {
        // rgb500 = pure red → nearest 16-color is bright red (9)
        let s = TfStr::from_tf_markup("@{Crgb500}x");
        let attrs = s.char_attrs().unwrap();
        assert_eq!(attrs[0].fg_color(), Some(color::BRIGHT_RED));
    }

    #[test]
    fn from_tf_markup_rgb_black() {
        let s = TfStr::from_tf_markup("@{Cbgrgb000}x");
        let attrs = s.char_attrs().unwrap();
        // rgb000 → black (0)
        assert_eq!(attrs[0].bg_color(), Some(0));
    }

    #[test]
    fn from_tf_markup_empty_resets() {
        let s = TfStr::from_tf_markup("@{B}a@{}b");
        let attrs = s.char_attrs().unwrap();
        assert_eq!(attrs[0], Attr::BOLD);
        assert_eq!(attrs[1], Attr::EMPTY);
    }

    #[test]
    fn from_tf_markup_strips_sequences_from_text() {
        let s = TfStr::from_tf_markup("@{B}hello@{n} world");
        assert_eq!(s.data, "hello world");
    }

    fn attrs_for(s: &TfStr) -> Vec<Attr> {
        s.chars_and_attrs().map(|(_, a)| a).collect()
    }

    #[test]
    fn display_impl() {
        let s = TfStr::from("world");
        assert_eq!(format!("{s}"), "world");
    }
}
