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
    fn display_impl() {
        let s = TfStr::from("world");
        assert_eq!(format!("{s}"), "world");
    }
}
