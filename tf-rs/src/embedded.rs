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
