//! Embedded copies of the `lib/tf/` library files.
//!
//! All `.tf` files from the repository's `lib/tf/` directory are baked into
//! the binary at compile time via `include_bytes!()`.  This allows the binary
//! to work without any installed lib directory (e.g. after `cargo install`).
//!
//! # Resolution order (see `cli::resolve_libdir`)
//! 1. `-L<dir>` CLI flag              → load from that directory on disk
//! 2. `$TFLIBDIR` env var             → load from that directory on disk
//! 3. `CARGO_MANIFEST_DIR/../lib/tf`  → load from repo (dev builds only)
//! 4. OS user data dir (`~/.local/share/tf` on Linux, etc.)  → load from disk
//! 5. **These embedded files**        → load from binary (no disk access)

/// A single embedded library file.
pub struct EmbeddedFile {
    pub name: &'static str,
    pub content: &'static [u8],
}

/// The embedded help text and its index.
pub static EMBEDDED_HELP: &[u8]     = include_bytes!("../../lib/tf/tf-help");
pub static EMBEDDED_HELP_IDX: &[u8] = include_bytes!("../../lib/tf/tf-help.idx");

/// All embedded `.tf` files from `lib/tf/`.
pub static EMBEDDED_LIBS: &[EmbeddedFile] = &[
    EmbeddedFile { name: "activity_status.tf",  content: include_bytes!("../../lib/tf/activity_status.tf") },
    EmbeddedFile { name: "activity_status2.tf", content: include_bytes!("../../lib/tf/activity_status2.tf") },
    EmbeddedFile { name: "alias.tf",            content: include_bytes!("../../lib/tf/alias.tf") },
    EmbeddedFile { name: "at.tf",               content: include_bytes!("../../lib/tf/at.tf") },
    EmbeddedFile { name: "changes.tf",          content: include_bytes!("../../lib/tf/changes.tf") },
    EmbeddedFile { name: "color.tf",            content: include_bytes!("../../lib/tf/color.tf") },
    EmbeddedFile { name: "complete.tf",         content: include_bytes!("../../lib/tf/complete.tf") },
    EmbeddedFile { name: "cylon.tf",            content: include_bytes!("../../lib/tf/cylon.tf") },
    EmbeddedFile { name: "factoral.tf",         content: include_bytes!("../../lib/tf/factoral.tf") },
    EmbeddedFile { name: "filexfer.tf",         content: include_bytes!("../../lib/tf/filexfer.tf") },
    EmbeddedFile { name: "finger.tf",           content: include_bytes!("../../lib/tf/finger.tf") },
    EmbeddedFile { name: "grep.tf",             content: include_bytes!("../../lib/tf/grep.tf") },
    EmbeddedFile { name: "hanoi.tf",            content: include_bytes!("../../lib/tf/hanoi.tf") },
    EmbeddedFile { name: "kb-bash.tf",          content: include_bytes!("../../lib/tf/kb-bash.tf") },
    EmbeddedFile { name: "kb-emacs.tf",         content: include_bytes!("../../lib/tf/kb-emacs.tf") },
    EmbeddedFile { name: "kb-old.tf",           content: include_bytes!("../../lib/tf/kb-old.tf") },
    EmbeddedFile { name: "kb-os2.tf",           content: include_bytes!("../../lib/tf/kb-os2.tf") },
    EmbeddedFile { name: "kb_badterm.tf",       content: include_bytes!("../../lib/tf/kb_badterm.tf") },
    EmbeddedFile { name: "kbbind.tf",           content: include_bytes!("../../lib/tf/kbbind.tf") },
    EmbeddedFile { name: "kbfunc.tf",           content: include_bytes!("../../lib/tf/kbfunc.tf") },
    EmbeddedFile { name: "kbregion.tf",         content: include_bytes!("../../lib/tf/kbregion.tf") },
    EmbeddedFile { name: "kbstack.tf",          content: include_bytes!("../../lib/tf/kbstack.tf") },
    EmbeddedFile { name: "lisp.tf",             content: include_bytes!("../../lib/tf/lisp.tf") },
    EmbeddedFile { name: "local-eg.tf",         content: include_bytes!("../../lib/tf/local-eg.tf") },
    EmbeddedFile { name: "map.tf",              content: include_bytes!("../../lib/tf/map.tf") },
    EmbeddedFile { name: "pcmd.tf",             content: include_bytes!("../../lib/tf/pcmd.tf") },
    EmbeddedFile { name: "psh.tf",              content: include_bytes!("../../lib/tf/psh.tf") },
    EmbeddedFile { name: "quoter.tf",           content: include_bytes!("../../lib/tf/quoter.tf") },
    EmbeddedFile { name: "relog.tf",            content: include_bytes!("../../lib/tf/relog.tf") },
    EmbeddedFile { name: "rwho.tf",             content: include_bytes!("../../lib/tf/rwho.tf") },
    EmbeddedFile { name: "savehist.tf",         content: include_bytes!("../../lib/tf/savehist.tf") },
    EmbeddedFile { name: "self.tf",             content: include_bytes!("../../lib/tf/self.tf") },
    EmbeddedFile { name: "spc-page.tf",         content: include_bytes!("../../lib/tf/spc-page.tf") },
    EmbeddedFile { name: "spedwalk.tf",         content: include_bytes!("../../lib/tf/spedwalk.tf") },
    EmbeddedFile { name: "spell.tf",            content: include_bytes!("../../lib/tf/spell.tf") },
    EmbeddedFile { name: "stack-q.tf",          content: include_bytes!("../../lib/tf/stack-q.tf") },
    EmbeddedFile { name: "stdlib.tf",           content: include_bytes!("../../lib/tf/stdlib.tf") },
    EmbeddedFile { name: "testcolor.tf",        content: include_bytes!("../../lib/tf/testcolor.tf") },
    EmbeddedFile { name: "textencode.tf",       content: include_bytes!("../../lib/tf/textencode.tf") },
    EmbeddedFile { name: "textutil.tf",         content: include_bytes!("../../lib/tf/textutil.tf") },
    EmbeddedFile { name: "tfstatus.tf",         content: include_bytes!("../../lib/tf/tfstatus.tf") },
    EmbeddedFile { name: "tick.tf",             content: include_bytes!("../../lib/tf/tick.tf") },
    EmbeddedFile { name: "tintin.tf",           content: include_bytes!("../../lib/tf/tintin.tf") },
    EmbeddedFile { name: "tools.tf",            content: include_bytes!("../../lib/tf/tools.tf") },
    EmbeddedFile { name: "tr.tf",               content: include_bytes!("../../lib/tf/tr.tf") },
    EmbeddedFile { name: "watch.tf",            content: include_bytes!("../../lib/tf/watch.tf") },
    EmbeddedFile { name: "world-q.tf",          content: include_bytes!("../../lib/tf/world-q.tf") },
];

/// Look up an embedded file by name, returning its content as UTF-8.
///
/// `name` should be a bare filename (e.g. `"stdlib.tf"`), not a path.
pub fn get_embedded(name: &str) -> Option<&'static str> {
    EMBEDDED_LIBS
        .iter()
        .find(|f| f.name == name)
        .map(|f| std::str::from_utf8(f.content)
            .unwrap_or_else(|_| panic!("embedded file '{name}' contains invalid UTF-8")))
}

/// Iterate over all embedded files as `(name, utf8_content)` pairs.
pub fn all_embedded() -> impl Iterator<Item = (&'static str, &'static str)> {
    EMBEDDED_LIBS.iter().map(|f| {
        let content = std::str::from_utf8(f.content)
            .unwrap_or_else(|_| panic!("embedded file '{}' contains invalid UTF-8", f.name));
        (f.name, content)
    })
}

// ── Help file lookup ──────────────────────────────────────────────────────────

/// Result of a successful help lookup.
#[derive(Debug)]
pub struct HelpResult {
    /// E.g. `"Help on: /def"` or `"Help on: /def: -m"`.
    pub header: String,
    /// Content lines, ANSI escape sequences stripped.
    pub lines: Vec<String>,
    /// Set when a subtopic was matched: `"For more complete information, see \"/def\"."`.
    pub footnote: Option<String>,
}

/// Look up a help topic using the embedded index and help text.
///
/// `topic` may be a command name with or without a leading `/`, e.g. `"def"` or `"/def"`.
/// Returns `Err(message)` if the topic is not found or help data is unavailable.
pub fn lookup_help(topic: &str) -> Result<HelpResult, String> {
    let idx = std::str::from_utf8(EMBEDDED_HELP_IDX)
        .map_err(|_| "tf-help.idx: invalid UTF-8".to_string())?;
    let content = std::str::from_utf8(EMBEDDED_HELP)
        .map_err(|_| "tf-help: invalid UTF-8".to_string())?;
    lookup_help_in(topic, idx, content)
        .ok_or_else(|| format!("% Help on subject {topic} not found."))
}

#[allow(unused_assignments)] // current_minor initial value is overwritten before first read
fn lookup_help_in(topic: &str, idx: &str, content: &str) -> Option<HelpResult> {
    let mut found: Option<(usize, &str, &str)> = None; // (offset, major_line, minor_line)
    let mut current_major = "";
    let mut current_minor: Option<&str> = None;

    for line in idx.lines() {
        // Find the first non-digit character (the separator: & or #)
        let sep_pos = line.find(|c: char| !c.is_ascii_digit())?;
        let sep = line.as_bytes()[sep_pos];
        let offset_str = &line[..sep_pos];
        let name = &line[sep_pos + 1..]; // topic name after & or #

        if sep == b'&' {
            current_minor = None;
            current_major = line;
        } else if sep == b'#' {
            current_minor = Some(line);
        } else {
            continue;
        }

        // C TF matching: exact match, OR first char is punctuation and tail matches.
        // This lets "connect" match the "/connect" entry.
        let is_match = name == topic
            || (!name.is_empty()
                && name.as_bytes()[0].is_ascii_punctuation()
                && &name[1..] == topic);

        if is_match {
            let offset: usize = offset_str.parse().ok()?;
            found = Some((offset, current_major, current_minor.unwrap_or("")));
            break;
        }
    }

    let (location, match_major, match_minor) = found?;

    // The help file is pure ASCII so byte offset == char boundary always.
    if location > content.len() {
        return None;
    }
    let section = &content[location..];

    // Skip leading & and # header lines; remember last major/minor from them.
    let mut header_major = match_major;
    let mut header_minor = match_minor;
    let mut lines_iter = section.lines().peekable();
    let first_content_line = loop {
        match lines_iter.next() {
            None => return None,
            Some(line) => {
                if line.starts_with('&') {
                    header_major = line; // update from the section itself
                } else if line.starts_with('#') {
                    // only update minor if we haven't matched one already
                    if header_minor.is_empty() {
                        header_minor = line;
                    }
                } else {
                    break line;
                }
            }
        }
    };

    let major_name = parse_topic_name(header_major);
    let minor_name = if !header_minor.is_empty() {
        Some(parse_topic_name(header_minor))
    } else {
        None
    };

    let header = match &minor_name {
        Some(minor) => format!("Help on: {major_name}: {minor}"),
        None => format!("Help on: {major_name}"),
    };

    // Collect content lines until the next & section boundary.
    let mut out_lines = Vec::new();
    out_lines.push(strip_ansi(first_content_line));
    for line in lines_iter {
        if line.starts_with('&') {
            break;
        }
        // C TF: if matched a minor subtopic, stop at the next # subtopic boundary too.
        if line.starts_with('#') {
            if minor_name.is_some() {
                break;
            }
            continue; // skip # lines that are just sub-markers within the section
        }
        out_lines.push(strip_ansi(line));
    }

    let footnote = minor_name
        .as_deref()
        .map(|_| format!("For more complete information, see \"{major_name}\"."));

    Some(HelpResult { header, lines: out_lines, footnote })
}

/// Extract the topic name from a raw index/header line like `"6063&/addtiny"` or `"24791#/def -m"`.
fn parse_topic_name(s: &str) -> String {
    match s.find(|c: char| !c.is_ascii_digit()) {
        Some(pos) if pos < s.len() => s[pos + 1..].to_owned(),
        _ => s.to_owned(),
    }
}

/// Strip ANSI SGR escape sequences (`ESC [ ... m`) from a string.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        // ESC [ … m  →  skip the whole sequence
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            i += 1; // skip 'm'
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lookup_by_slash_command() {
        let result = lookup_help("/def").expect("/def should be in help");
        assert!(result.header.contains("/def"), "header: {}", result.header);
        assert!(!result.lines.is_empty(), "content should not be empty");
    }

    #[test]
    fn help_lookup_without_slash() {
        let result = lookup_help("connect").expect("connect should be in help");
        assert!(result.header.contains("connect"), "header: {}", result.header);
    }

    #[test]
    fn help_lookup_summary() {
        let result = lookup_help("summary").expect("summary should be in help");
        assert!(result.header.contains("summary") || result.header.contains("Summary"),
            "header: {}", result.header);
    }

    #[test]
    fn help_lookup_not_found() {
        let err = lookup_help("no_such_topic_xyzzy").unwrap_err();
        assert!(err.contains("not found"), "error: {err}");
    }

    #[test]
    fn help_lookup_subtopic() {
        // /def -m is indexed as a subtopic (#)
        let result = lookup_help("/def -m").expect("/def -m subtopic should be in help");
        // Should have a footnote pointing back to the main /def topic
        assert!(result.footnote.is_some(), "subtopic should have footnote");
    }
}
