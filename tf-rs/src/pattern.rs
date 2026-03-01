//! Pattern matching — regex, glob, simple substring, and exact modes.
//!
//! Corresponds to `pattern.c` / `pattern.h` in the C source.  The public API
//! mirrors the C `Pattern` / `patmatch` / `regsubstr` surface while using
//! idiomatic Rust types.
//!
//! ## Match modes
//!
//! | Mode | C flag | Description |
//! |------|--------|-------------|
//! | [`MatchMode::Regexp`] | `MATCH_REGEXP` | PCRE2 → [`regex`] crate |
//! | [`MatchMode::Glob`]   | `MATCH_GLOB`   | TF glob (`*`, `?`, `[…]`, `{a\|b}`) |
//! | [`MatchMode::Simple`] | `MATCH_SIMPLE` | Case-insensitive exact match |
//! | [`MatchMode::Substr`] | `MATCH_SUBSTR` | Case-insensitive substring search |

use std::sync::Arc;

use regex::Regex;

// ── Public types ─────────────────────────────────────────────────────────────

/// Which matching algorithm a [`Pattern`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    Regexp,
    Glob,
    Simple,
    Substr,
}

/// Error returned when a pattern cannot be compiled.
#[derive(Debug)]
pub enum PatternError {
    InvalidRegex(regex::Error),
    InvalidGlob(String),
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternError::InvalidRegex(e) => write!(f, "regex error: {e}"),
            PatternError::InvalidGlob(msg) => write!(f, "glob error: {msg}"),
        }
    }
}

impl std::error::Error for PatternError {}

// Compiled form — kept private; callers use Pattern's methods.
// Arc wrappers make Clone a cheap reference-count increment instead of a recompile.
#[derive(Clone)]
enum Compiled {
    Regex(Arc<Regex>),
    Glob,
    Simple(String),
    /// Stores the pattern lowercased for O(n·m) case-insensitive substring search.
    /// AhoCorasick would be better for many patterns simultaneously, but for a
    /// single trigger pattern the AC state-machine build overhead is not worth it.
    Substr(String),
}

/// A compiled pattern ready for matching.
#[derive(Clone)]
pub struct Pattern {
    src: String,
    mode: MatchMode,
    compiled: Compiled,
}

impl std::fmt::Debug for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pattern")
            .field("src", &self.src)
            .field("mode", &self.mode)
            .finish()
    }
}

impl Pattern {
    /// Compile `src` using `mode`.
    ///
    /// Returns [`PatternError`] if the pattern is syntactically invalid.
    pub fn new(src: &str, mode: MatchMode) -> Result<Self, PatternError> {
        let compiled = match mode {
            MatchMode::Regexp => Compiled::Regex(Arc::new(compile_regex(src)?)),
            MatchMode::Glob => {
                check_glob(src).map_err(PatternError::InvalidGlob)?;
                Compiled::Glob
            }
            MatchMode::Simple => Compiled::Simple(src.to_ascii_lowercase()),
            MatchMode::Substr => Compiled::Substr(src.to_ascii_lowercase()),
        };
        Ok(Self {
            src: src.to_owned(),
            mode,
            compiled,
        })
    }

    /// The original source string.
    pub fn src(&self) -> &str {
        &self.src
    }

    /// The match mode.
    pub fn mode(&self) -> MatchMode {
        self.mode
    }

    /// Returns `true` if this pattern matches `text`.
    ///
    /// An empty source string always matches (C: `if (!pat->str) return 1`).
    pub fn matches(&self, text: &str) -> bool {
        if self.src.is_empty() {
            return true;
        }
        match &self.compiled {
            Compiled::Regex(re) => re.is_match(text),
            Compiled::Glob => glob_match(&self.src, text),
            Compiled::Simple(lo) => {
                if text.len() != lo.len() {
                    false
                } else {
                    text.as_bytes()
                        .iter()
                        .zip(lo.as_bytes())
                        .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
                }
            }
            Compiled::Substr(lo) => substr_find_ascii_ci(text, lo).is_some(),
        }
    }

    /// Attempt a match and return [`Captures`] on success.
    ///
    /// For non-Regexp modes the captures object gives access to `left`,
    /// `whole`, and `right` but has no numbered capture groups.
    pub fn find<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        if self.src.is_empty() {
            return Some(Captures {
                text,
                start: 0,
                end: 0,
                groups: vec![],
            });
        }
        match &self.compiled {
            Compiled::Regex(re) => {
                let caps = re.captures(text)?;
                let whole = caps.get(0).unwrap();
                let groups = (1..caps.len())
                    .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
                    .collect();
                Some(Captures {
                    text,
                    start: whole.start(),
                    end: whole.end(),
                    groups,
                })
            }
            Compiled::Substr(lo) => {
                substr_find_ascii_ci(text, lo).map(|(start, end)| Captures {
                    text,
                    start,
                    end,
                    groups: vec![],
                })
            }
            _ => {
                if self.matches(text) {
                    Some(Captures {
                        text,
                        start: 0,
                        end: text.len(),
                        groups: vec![],
                    })
                } else {
                    None
                }
            }
        }
    }
}

/// The result of a successful pattern match with access to capture groups.
///
/// Corresponds to the `regsubstr` family of C functions (n ≥ 0 for a
/// numbered group, −1 for left, −2 for right).
pub struct Captures<'t> {
    text: &'t str,
    start: usize,
    end: usize,
    /// (start, end) byte offsets per capture group; `None` = group didn't participate.
    groups: Vec<Option<(usize, usize)>>,
}

impl<'t> Captures<'t> {
    /// Text before the match (C: `regsubstr(dest, -1)`).
    pub fn left(&self) -> &'t str {
        &self.text[..self.start]
    }

    /// The entire matched substring (C: capture group 0).
    pub fn whole(&self) -> &'t str {
        &self.text[self.start..self.end]
    }

    /// Text after the match (C: `regsubstr(dest, -2)`).
    pub fn right(&self) -> &'t str {
        &self.text[self.end..]
    }

    /// The nth capture group, 1-based (C: `regsubstr(dest, n)` with n > 0).
    pub fn group(&self, n: usize) -> Option<&'t str> {
        self.groups
            .get(n.checked_sub(1)?)?
            .as_ref()
            .map(|&(s, e)| &self.text[s..e])
    }

    /// Number of capture groups (excluding the overall match).
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
}

// ── Regex compilation ─────────────────────────────────────────────────────────

/// Compile a regex, replicating the C PCRE2 options:
/// - Case-insensitive by default; disabled if the pattern contains an
///   unescaped uppercase letter.
/// - `.` matches newlines (`PCRE2_DOTALL`).
/// - `$` matches only at the true end of the string (`PCRE2_DOLLAR_ENDONLY`).
fn compile_regex(pattern: &str) -> Result<Regex, PatternError> {
    let case_insensitive = !has_unescaped_upper(pattern);
    regex::RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .dot_matches_new_line(true)
        .multi_line(false)
        .build()
        .map_err(PatternError::InvalidRegex)
}

fn has_unescaped_upper(pattern: &str) -> bool {
    let mut escaped = false;
    let mut in_bracket = false;
    for ch in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if in_bracket {
            if ch == ']' {
                in_bracket = false;
            }
            // Don't treat uppercase inside [...] as "has unescaped upper" —
            // bracket expressions like [A-Z] are character-class matchers, not
            // literal uppercase letters that would disable case-insensitivity.
            continue;
        }
        if ch == '[' {
            in_bracket = true;
            continue;
        }
        if ch.is_uppercase() {
            return true;
        }
    }
    false
}

// ── Glob matching ─────────────────────────────────────────────────────────────
//
// Port of the C `smatch` / `smatch_check` functions (pattern.c).
//
// Glob syntax:
//   *        — any sequence of characters (not spaces in word mode)
//   ?        — any single character (not space in word mode)
//   [...]    — character class, case-insensitive; [^...] negated
//   {a|b|c}  — word alternatives; must appear at a word boundary;
//              matches the next space-delimited token against each alternative
//   \x       — literal x
//   All matching is case-insensitive.

/// Maximum recursion depth for the glob matcher.
///
/// The `*` wildcard in `smatch` causes one recursive call per character
/// position in the remaining text, and each recursive call can itself
/// encounter another `*`.  A pathological pattern like `*a*a*a*b` with a
/// long text of `a`s can thus produce O(2ⁿ) calls.
///
/// This limit caps the recursion; when exceeded the match returns `false`
/// (conservative: the pattern might have matched, but a hang is worse).
/// 256 levels allows patterns with up to ~16 `*` wildcards against typical
/// MUD lines (< 512 chars) without false negatives in practice.
const SMATCH_MAX_DEPTH: usize = 256;

/// Public entry point for glob matching.
pub fn glob_match(pat: &str, text: &str) -> bool {
    let p = pat.as_bytes();
    let s = text.as_bytes();
    smatch(p, s, s, false, SMATCH_MAX_DEPTH)
}

/// Recursive glob matcher.  Returns `true` on match.
///
/// `start` is the original beginning of the subject string (never advanced),
/// used to detect word boundaries.  `inword` is `true` inside `{...}`.
/// `depth` is the remaining recursion budget; returns `false` when exhausted.
fn smatch(mut pat: &[u8], mut s: &[u8], start: &[u8], inword: bool, depth: usize) -> bool {
    if depth == 0 {
        return false; // recursion limit reached — treat as no-match
    }
    loop {
        let p = match pat.first().copied() {
            None => {
                // End of pattern.
                return if inword {
                    // In word mode success means we consumed the whole word.
                    s.is_empty() || s[0] == b' '
                } else {
                    s.is_empty()
                };
            }
            Some(p) => p,
        };

        match p {
            b'\\' => {
                // Escaped literal.
                if pat.len() < 2 {
                    return s.is_empty();
                }
                match s.first() {
                    Some(&c) if c.eq_ignore_ascii_case(&pat[1]) => {}
                    _ => return false,
                }
                pat = &pat[2..];
                s = &s[1..];
            }

            b'?' => {
                match s.first() {
                    None => return false,
                    Some(&b' ') if inword => return false,
                    _ => {}
                }
                s = &s[1..];
                pat = &pat[1..];
            }

            b'*' => {
                // Consume consecutive `*` and `?` wildcards.
                while let Some(&(b'*' | b'?')) = pat.first() {
                    if pat[0] == b'?' {
                        match s.first() {
                            None => return false,
                            Some(&b' ') if inword => return false,
                            _ => {}
                        }
                        s = &s[1..];
                    }
                    pat = &pat[1..];
                }

                if inword {
                    // `*` inside `{…}`: match up to but not including space.
                    let mut pos = s;
                    while !pos.is_empty() && pos[0] != b' ' {
                        if smatch(pat, pos, start, inword, depth - 1) {
                            return true;
                        }
                        pos = &pos[1..];
                    }
                    return smatch(pat, pos, start, inword, depth - 1);
                } else if pat.is_empty() {
                    return true;
                } else if pat[0] == b'{' {
                    // `*` before a word-group: try at every word boundary.
                    let s_off = start.len() - s.len();
                    if (s_off == 0 || start[s_off - 1] == b' ') && smatch(pat, s, start, inword, depth - 1) {
                        return true;
                    }
                    let mut pos = s;
                    while !pos.is_empty() {
                        if pos[0] == b' ' && smatch(pat, &pos[1..], start, inword, depth - 1) {
                            return true;
                        }
                        pos = &pos[1..];
                    }
                    return false;
                } else if pat[0] == b'[' {
                    let mut pos = s;
                    while !pos.is_empty() {
                        if smatch(pat, pos, start, inword, depth - 1) {
                            return true;
                        }
                        pos = &pos[1..];
                    }
                    return false;
                } else {
                    // Optimisation: skip to next occurrence of the first literal.
                    let first = if pat[0] == b'\\' && pat.len() > 1 {
                        pat[1]
                    } else {
                        pat[0]
                    };
                    let first_lo = first.to_ascii_lowercase();
                    let mut pos = s;
                    while !pos.is_empty() {
                        if pos[0].to_ascii_lowercase() == first_lo
                            && smatch(pat, pos, start, inword, depth - 1)
                        {
                            return true;
                        }
                        pos = &pos[1..];
                    }
                    return false;
                }
            }

            b'[' => {
                if inword && s.first() == Some(&b' ') {
                    return false;
                }
                let ch = match s.first().copied() {
                    Some(c) => c,
                    None => return false,
                };
                match cmatch(&pat[1..], ch) {
                    None => return false,
                    Some(rest) => {
                        pat = rest;
                        s = &s[1..];
                    }
                }
            }

            b'{' => {
                // Word-group: must be at a word boundary.
                let s_off = start.len() - s.len();
                if s_off != 0 && start[s_off - 1] != b' ' {
                    return false;
                }

                let close = match find_unescaped(pat, b'}') {
                    Some(i) => i,
                    None => return false, // malformed; should be caught by check_glob
                };

                let inner = &pat[1..close];
                let after = &pat[close + 1..];

                // Try each `|`-separated alternative.
                let mut alts = inner;
                let mut matched = false;
                loop {
                    let pipe = find_unescaped(alts, b'|').unwrap_or(alts.len());
                    let alt = &alts[..pipe];
                    if smatch(alt, s, start, true, depth - 1) {
                        matched = true;
                        break;
                    }
                    if pipe >= alts.len() {
                        break;
                    }
                    alts = &alts[pipe + 1..];
                }
                if !matched {
                    return false;
                }

                // Consume the rest of the current word in the subject.
                while !s.is_empty() && s[0] != b' ' {
                    s = &s[1..];
                }
                pat = after;
            }

            b'}' | b'|' if inword => {
                // Terminator of a word alternative.
                return s.is_empty() || s[0] == b' ';
            }

            _ => {
                match s.first() {
                    Some(&c) if c.eq_ignore_ascii_case(&p) => {}
                    _ => return false,
                }
                pat = &pat[1..];
                s = &s[1..];
            }
        }
    }
}

/// Match a character class `[…]` against `ch`.
///
/// `class` is the slice *after* the opening `[`.
/// Returns the slice after the closing `]` on match, `None` on non-match.
fn cmatch(mut class: &[u8], ch: u8) -> Option<&[u8]> {
    let ch = ch.to_ascii_lowercase();
    let negated = class.first() == Some(&b'^');
    if negated {
        class = &class[1..];
    }

    let mut matched = false;
    loop {
        match class.first().copied() {
            None => return None, // malformed
            Some(b']') => break,
            Some(b'\\') if class.len() > 1 => {
                if class[1].to_ascii_lowercase() == ch {
                    matched = true;
                }
                class = &class[2..];
            }
            Some(lo) => {
                // Check for a range `lo-hi`.
                if class.len() >= 3 && class[1] == b'-' && class[2] != b']' {
                    let hi_raw = if class[2] == b'\\' && class.len() > 3 {
                        class = &class[1..]; // skip extra backslash
                        class[2]
                    } else {
                        class[2]
                    };
                    if ch >= lo.to_ascii_lowercase() && ch <= hi_raw.to_ascii_lowercase() {
                        matched = true;
                    }
                    class = &class[3..];
                } else {
                    if lo.to_ascii_lowercase() == ch {
                        matched = true;
                    }
                    class = &class[1..];
                }
            }
        }
    }

    // `class` now points at `]`; advance past it.
    let rest = &class[1..];
    if matched ^ negated {
        Some(rest)
    } else {
        None
    }
}

/// Find the first unescaped occurrence of `needle` in `haystack`.
fn find_unescaped(haystack: &[u8], needle: u8) -> Option<usize> {
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i] == b'\\' {
            i += 2;
            continue;
        }
        if haystack[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// ASCII-case-insensitive substring search.
///
/// Returns `Some((start, end))` byte offsets of the first occurrence of
/// `lo_pattern` (which **must** already be ASCII-lowercased) in `text`.
///
/// This is an O(n·m) scan — acceptable for typical trigger patterns (short
/// patterns, moderate text length).  For searching many patterns at once,
/// use `aho_corasick` instead.
fn substr_find_ascii_ci(text: &str, lo_pattern: &str) -> Option<(usize, usize)> {
    let tb = text.as_bytes();
    let pb = lo_pattern.as_bytes();
    if pb.is_empty() {
        return Some((0, 0));
    }
    'outer: for i in 0..=tb.len().saturating_sub(pb.len()) {
        for (j, &p) in pb.iter().enumerate() {
            if tb[i + j].to_ascii_lowercase() != p {
                continue 'outer;
            }
        }
        return Some((i, i + pb.len()));
    }
    None
}

// ── Glob syntax validation ────────────────────────────────────────────────────

/// Validate glob pattern syntax.  Mirrors the C `smatch_check` function.
///
/// Returns `Ok(())` if valid, `Err(description)` otherwise.
pub fn check_glob(pat: &str) -> Result<(), String> {
    let mut inword = false;
    let pat_start = pat;
    let mut chars = pat.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            } // skip escaped char
            '[' => {
                // scan to matching ]
                let mut found = false;
                let iter = chars.by_ref();
                while let Some((_, c)) = iter.next() {
                    if c == '\\' {
                        iter.next();
                        continue;
                    }
                    if c == ']' {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err("unmatched '['".into());
                }
            }
            '{' => {
                if inword {
                    return Err("nested '{'".into());
                }
                // preceding character must be start, space, *, ?, or ]
                let prev = pat_start[..i].chars().next_back();
                if let Some(p) = prev {
                    if !matches!(p, ' ' | '*' | '?' | ']') {
                        return Err(format!("'{p}' before '{{' can never match"));
                    }
                }
                inword = true;
            }
            '}' => {
                // following character must be end, space, *, ?, or [
                let rest = &pat[i + 1..];
                if let Some(next) = rest.chars().next() {
                    if !matches!(next, ' ' | '*' | '?' | '[') {
                        return Err(format!("'{next}' after '}}' can never match"));
                    }
                }
                inword = false;
            }
            ' ' if inword => {
                return Err("space inside '{...}' can never match".into());
            }
            _ => {}
        }
    }
    if inword {
        return Err("unmatched '{'".into());
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- Regex ----------------------------------------------------------------

    #[test]
    fn regex_basic_match() {
        let p = Pattern::new("hello", MatchMode::Regexp).unwrap();
        assert!(p.matches("say hello world"));
        assert!(!p.matches("goodbye"));
    }

    #[test]
    fn regex_case_insensitive_by_default() {
        let p = Pattern::new("hello", MatchMode::Regexp).unwrap();
        assert!(p.matches("HELLO"));
    }

    #[test]
    fn regex_uppercase_enables_case_sensitivity() {
        let p = Pattern::new("Hello", MatchMode::Regexp).unwrap();
        assert!(p.matches("Hello"));
        assert!(!p.matches("hello"));
    }

    #[test]
    fn regex_bracket_class_does_not_disable_case_insensitivity() {
        // [A-Z] contains uppercase letters but they are inside a bracket class;
        // the overall pattern should still match case-insensitively outside the class.
        let p = Pattern::new(r"[a-z]+", MatchMode::Regexp).unwrap();
        assert!(p.matches("HELLO"), "pattern with only lowercase bracket class should be case-insensitive");
        // A bracket class with uppercase should enable sensitivity only for that part.
        let p2 = Pattern::new(r"[A-Z]+", MatchMode::Regexp).unwrap();
        assert!(p2.matches("hello"), "bracket [A-Z] should not suppress global case-insensitivity");
    }

    #[test]
    fn regex_capture_groups() {
        let p = Pattern::new(r"(\w+)\s+(\w+)", MatchMode::Regexp).unwrap();
        let caps = p.find("foo bar baz").unwrap();
        assert_eq!(caps.whole(), "foo bar");
        assert_eq!(caps.group(1), Some("foo"));
        assert_eq!(caps.group(2), Some("bar"));
        assert_eq!(caps.left(), "");
        assert_eq!(caps.right(), " baz");
    }

    #[test]
    fn regex_dot_matches_newline() {
        let p = Pattern::new("a.b", MatchMode::Regexp).unwrap();
        assert!(p.matches("a\nb"));
    }

    #[test]
    fn empty_pattern_always_matches() {
        for mode in [
            MatchMode::Regexp,
            MatchMode::Glob,
            MatchMode::Simple,
            MatchMode::Substr,
        ] {
            let p = Pattern::new("", mode).unwrap();
            assert!(p.matches("anything"), "mode {mode:?} failed");
            assert!(p.matches(""), "mode {mode:?} failed on empty");
        }
    }

    // -- Glob ----------------------------------------------------------------

    #[test]
    fn glob_star_matches_anything() {
        assert!(glob_match("*", "hello world"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("h?llo", "hello"));
        assert!(!glob_match("h?llo", "hllo"));
    }

    #[test]
    fn glob_literal_case_insensitive() {
        assert!(glob_match("Hello", "HELLO"));
        assert!(glob_match("hello", "Hello"));
    }

    #[test]
    fn glob_star_prefix_suffix() {
        assert!(glob_match("*world*", "hello world!"));
        assert!(!glob_match("*world*", "hello earth!"));
    }

    #[test]
    fn glob_character_class() {
        assert!(glob_match("[aeiou]nce", "once"));
        assert!(glob_match("[aeiou]nce", "ance"));
        assert!(!glob_match("[aeiou]nce", "bnce"));
    }

    #[test]
    fn glob_character_class_range() {
        assert!(glob_match("[a-z]ello", "hello"));
        assert!(!glob_match("[a-z]ello", "1ello"));
    }

    #[test]
    fn glob_character_class_negated() {
        assert!(glob_match("[^aeiou]ello", "hello"));
        assert!(!glob_match("[^aeiou]ello", "aello"));
    }

    #[test]
    fn glob_word_group_simple() {
        assert!(glob_match("* {north|south|east|west}*", "go north"));
        assert!(glob_match("* {north|south|east|west}*", "go south now"));
        assert!(!glob_match("* {north|south|east|west}*", "go nowhere"));
    }

    #[test]
    fn glob_word_group_exact_word() {
        assert!(glob_match("{hello}", "hello"));
        assert!(!glob_match("{hello}", "hello world"));
        assert!(!glob_match("{hello}", "hell"));
    }

    #[test]
    fn glob_escape() {
        assert!(glob_match(r"a\*b", "a*b"));
        assert!(!glob_match(r"a\*b", "axb"));
    }

    // -- Simple / Substr -----------------------------------------------------

    #[test]
    fn simple_exact_match() {
        let p = Pattern::new("hello", MatchMode::Simple).unwrap();
        assert!(p.matches("Hello"));
        assert!(!p.matches("hello world"));
    }

    #[test]
    fn substr_match() {
        let p = Pattern::new("ello", MatchMode::Substr).unwrap();
        assert!(p.matches("Hello World"));
        assert!(!p.matches("Hi World"));
    }

    // -- check_glob ----------------------------------------------------------

    #[test]
    fn check_glob_valid() {
        assert!(check_glob("*").is_ok());
        assert!(check_glob("* {north|south}").is_ok());
        assert!(check_glob("[a-z]*").is_ok());
    }

    #[test]
    fn check_glob_unmatched_bracket() {
        assert!(check_glob("[abc").is_err());
    }

    #[test]
    fn check_glob_unmatched_brace() {
        assert!(check_glob("{north").is_err());
    }

    #[test]
    fn simple_mode_non_ascii_case() {
        let p = Pattern::new("é", MatchMode::Simple).unwrap();
        // `MatchMode::Simple` is ASCII-only case-insensitive; Unicode
        // case folding is not performed, so upper/lower variants won't match.
        assert!(!p.matches("É"));
    }

    #[test]
    fn glob_depth_limit_does_not_hang() {
        // Pathological pattern "*a*a*a..." against a long non-matching string
        // would be O(2ⁿ) without the depth limit. The depth limit must return
        // false quickly rather than hanging.
        let pattern = "*a".repeat(20);
        let text = "b".repeat(100);
        // Must complete essentially instantly; result is false (no match).
        assert!(!glob_match(&pattern, &text));
    }
}
