//! Key binding dispatch: `DoKeyOp` enum and `Keymap`.
//!
//! Corresponds to `keylist.h` and the key-binding layer of `keyboard.c`.
//!
//! ## Key sequence format
//!
//! Keys are stored as raw byte sequences (`Vec<u8>`), matching how the C code
//! stores them in the keybinding trie.  Common control characters are
//! expressed as byte values (e.g. `\x01` for Ctrl-A).  The helper
//! [`key_sequence`] converts printable escape notation for tests.

use std::collections::HashMap;

use crate::input::LineEditor;
use crate::history::{InputHistory, RecallMode};

// ── DoKeyOp ───────────────────────────────────────────────────────────────────

/// Built-in editor operations, corresponding to `DOKEY_*` in `keyboard.c`
/// and the entries in `keylist.h`.
///
/// These are the operations that can be triggered by `/dokey <name>` in TF
/// scripts or bound directly to key sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DoKeyOp {
    /// Erase the visible screen (`/dokey clear`).
    Clear,
    /// Flush all output to the terminal (`/dokey flush`).
    Flush,
    /// Treat the next character literally (`/dokey lnext` — like `C-v`).
    LiteralNext,
    /// Submit the current input line (`/dokey newline`).
    Newline,
    /// Pause output at a `--More--` prompt (`/dokey pause`).
    Pause,
    /// Recall the previous history entry (`/dokey recallb`).
    RecallBackward,
    /// Jump to the oldest history entry (`/dokey recallbeg`).
    RecallBeg,
    /// Jump to the newest history entry (`/dokey recallend`).
    RecallEnd,
    /// Recall the next history entry (`/dokey recallf`).
    RecallForward,
    /// Redraw the screen (`/dokey redraw`).
    Redraw,
    /// Refresh the input line (`/dokey refresh`).
    Refresh,
    /// Search backward in history using the current line as prefix (`/dokey searchb`).
    SearchBackward,
    /// Search forward in history using the current line as prefix (`/dokey searchf`).
    SearchForward,
    /// Selective flush (`/dokey selflush`).
    SelFlush,
}

impl DoKeyOp {
    /// The canonical name as used in TF scripts (`/dokey <name>`).
    pub fn name(self) -> &'static str {
        match self {
            DoKeyOp::Clear          => "CLEAR",
            DoKeyOp::Flush          => "FLUSH",
            DoKeyOp::LiteralNext    => "LNEXT",
            DoKeyOp::Newline        => "NEWLINE",
            DoKeyOp::Pause          => "PAUSE",
            DoKeyOp::RecallBackward => "RECALLB",
            DoKeyOp::RecallBeg      => "RECALLBEG",
            DoKeyOp::RecallEnd      => "RECALLEND",
            DoKeyOp::RecallForward  => "RECALLF",
            DoKeyOp::Redraw         => "REDRAW",
            DoKeyOp::Refresh        => "REFRESH",
            DoKeyOp::SearchBackward => "SEARCHB",
            DoKeyOp::SearchForward  => "SEARCHF",
            DoKeyOp::SelFlush       => "SELFLUSH",
        }
    }

    /// All variants, in the same order as `keylist.h`.
    pub const ALL: &'static [DoKeyOp] = &[
        DoKeyOp::Clear,
        DoKeyOp::Flush,
        DoKeyOp::LiteralNext,
        DoKeyOp::Newline,
        DoKeyOp::Pause,
        DoKeyOp::RecallBackward,
        DoKeyOp::RecallBeg,
        DoKeyOp::RecallEnd,
        DoKeyOp::RecallForward,
        DoKeyOp::Redraw,
        DoKeyOp::Refresh,
        DoKeyOp::SearchBackward,
        DoKeyOp::SearchForward,
        DoKeyOp::SelFlush,
    ];

    /// Parse a name (case-insensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        let upper = name.to_ascii_uppercase();
        Self::ALL.iter().copied().find(|op| op.name() == upper)
    }
}

// ── KeyBinding ────────────────────────────────────────────────────────────────

/// What a key sequence is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyBinding {
    /// A built-in editor operation.
    DoKey(DoKeyOp),
    /// A TF script body to execute (macro expansion happens in the interpreter).
    Macro(String),
}

// ── EditAction ────────────────────────────────────────────────────────────────

/// Immediate editing commands that the `Keymap` can emit from
/// [`Keymap::apply_default_bindings`].
///
/// These are handled by the event loop without going through the full macro
/// interpreter, mirroring the hard-wired handling in `handle_keyboard_input`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditAction {
    /// Insert this character at the cursor.
    InsertChar(char),
    /// Insert a string at the cursor (multi-char key).
    InsertStr(String),
    /// Backspace (delete before cursor).
    Backspace,
    /// Forward delete (delete at cursor).
    DeleteForward,
    /// Move left one character.
    MoveLeft,
    /// Move right one character.
    MoveRight,
    /// Move to start of line.
    MoveHome,
    /// Move to end of line.
    MoveEnd,
    /// Move one word backward.
    WordBackward,
    /// Move one word forward.
    WordForward,
    /// Kill to end of line.
    KillToEnd,
    /// Kill to start of line.
    KillToStart,
    /// Kill word forward.
    KillWordForward,
    /// Kill word backward.
    KillWordBackward,
    /// Yank from kill ring.
    Yank,
    /// A bound [`DoKeyOp`] or macro.
    Bound(KeyBinding),
}

// ── Keymap ────────────────────────────────────────────────────────────────────

/// Maps byte sequences to [`KeyBinding`]s.
///
/// The C source uses a trie for prefix-matching multi-byte escape sequences
/// while bytes arrive.  Here we use a `HashMap<Vec<u8>, KeyBinding>` for
/// simplicity; prefix detection is left to the event loop (Phase 9).
#[derive(Debug, Default)]
pub struct Keymap {
    bindings: HashMap<Vec<u8>, KeyBinding>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `sequence` to `binding`.  Returns `false` if the binding
    /// would conflict with an existing prefix (not implemented for
    /// HashMap — always succeeds, replacing the old binding).
    pub fn bind(&mut self, sequence: Vec<u8>, binding: KeyBinding) -> bool {
        self.bindings.insert(sequence, binding);
        true
    }

    /// Remove the binding for `sequence`.
    pub fn unbind(&mut self, sequence: &[u8]) {
        self.bindings.remove(sequence);
    }

    /// Look up a key sequence.
    pub fn lookup(&self, sequence: &[u8]) -> Option<&KeyBinding> {
        self.bindings.get(sequence)
    }

    /// Number of bindings in the map.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Populate with TF's standard default key bindings.
    ///
    /// These mirror the bindings from `stdlib.tf` that ship with TF and the
    /// hard-wired defaults in `keyboard.c`.
    pub fn with_defaults(mut self) -> Self {
        use DoKeyOp::*;

        // Control characters (Emacs-style defaults).
        let ctrl = |c: char| vec![c as u8 - b'@'];

        self.bind(ctrl('A'), KeyBinding::DoKey(Refresh));    // C-a: start of line (refresh in TF)
        self.bind(ctrl('B'), KeyBinding::Macro("/dokey_left".into()));
        self.bind(ctrl('D'), KeyBinding::Macro("/dokey_dch".into()));
        self.bind(ctrl('E'), KeyBinding::Macro("/dokey_end".into()));
        self.bind(ctrl('F'), KeyBinding::Macro("/dokey_right".into()));
        self.bind(ctrl('J'), KeyBinding::DoKey(Newline));
        self.bind(ctrl('K'), KeyBinding::Macro("/dokey_deol".into()));
        self.bind(ctrl('L'), KeyBinding::DoKey(Redraw));
        self.bind(ctrl('M'), KeyBinding::DoKey(Newline));
        self.bind(ctrl('N'), KeyBinding::DoKey(RecallForward));
        self.bind(ctrl('P'), KeyBinding::DoKey(RecallBackward));
        self.bind(ctrl('R'), KeyBinding::DoKey(SearchBackward));
        self.bind(ctrl('U'), KeyBinding::Macro("/dokey_debol".into()));
        self.bind(ctrl('V'), KeyBinding::DoKey(LiteralNext));
        self.bind(ctrl('W'), KeyBinding::Macro("/dokey_bword".into()));
        self.bind(ctrl('Y'), KeyBinding::Macro("/dokey_yank".into()));
        self.bind(ctrl('Z'), KeyBinding::DoKey(Pause));

        // Backspace and DEL.
        self.bind(vec![0x08], KeyBinding::Macro("/dokey_dch".into())); // C-h / BS
        self.bind(vec![0x7F], KeyBinding::Macro("/dokey_bch".into())); // DEL

        // Arrow keys (ANSI/VT100 sequences).
        self.bind(b"\x1b[A".to_vec(), KeyBinding::DoKey(RecallBackward));  // Up
        self.bind(b"\x1b[B".to_vec(), KeyBinding::DoKey(RecallForward));   // Down
        self.bind(b"\x1b[C".to_vec(), KeyBinding::Macro("/dokey_right".into())); // Right
        self.bind(b"\x1b[D".to_vec(), KeyBinding::Macro("/dokey_left".into()));  // Left
        self.bind(b"\x1b[H".to_vec(), KeyBinding::Macro("/dokey_home".into())); // Home
        self.bind(b"\x1b[F".to_vec(), KeyBinding::Macro("/dokey_end".into()));  // End

        // Alt+arrow (word movement).
        self.bind(b"\x1b[1;3C".to_vec(), KeyBinding::Macro("/dokey_fword".into())); // Alt+Right
        self.bind(b"\x1b[1;3D".to_vec(), KeyBinding::Macro("/dokey_bword".into())); // Alt+Left

        // History.
        self.bind(b"\x1b[5~".to_vec(), KeyBinding::DoKey(SearchBackward)); // PgUp
        self.bind(b"\x1b[6~".to_vec(), KeyBinding::DoKey(SearchForward));  // PgDn

        self
    }
}

// ── InputProcessor ────────────────────────────────────────────────────────────

/// Combines a [`LineEditor`] and [`InputHistory`] to process keyboard input.
///
/// This is the top-level input driver that the Phase 9 event loop will call.
/// It applies editing commands and recalls history as directed by
/// [`EditAction`] values.
pub struct InputProcessor {
    pub editor: LineEditor,
    pub history: InputHistory,
    pub literal_next: bool,
}

impl InputProcessor {
    pub fn new(history_size: usize) -> Self {
        Self {
            editor: LineEditor::new(),
            history: InputHistory::new(history_size),
            literal_next: false,
        }
    }

    /// Apply an [`EditAction`] to the editor/history state.
    ///
    /// Returns `Some(line)` when the user submits a line (Newline action),
    /// otherwise `None`.
    pub fn apply(&mut self, action: EditAction) -> Option<String> {
        match action {
            EditAction::InsertChar(ch) => {
                self.editor.insert_char(ch);
            }
            EditAction::InsertStr(s) => {
                self.editor.insert_str(&s);
            }
            EditAction::Backspace => {
                self.editor.delete_before();
            }
            EditAction::DeleteForward => {
                self.editor.delete_at();
            }
            EditAction::MoveLeft => {
                self.editor.move_left(1);
            }
            EditAction::MoveRight => {
                self.editor.move_right(1);
            }
            EditAction::MoveHome => {
                self.editor.move_home();
            }
            EditAction::MoveEnd => {
                self.editor.move_end();
            }
            EditAction::WordBackward => {
                self.editor.move_word_backward();
            }
            EditAction::WordForward => {
                self.editor.move_word_forward();
            }
            EditAction::KillToEnd => {
                self.editor.kill_to_end();
            }
            EditAction::KillToStart => {
                self.editor.kill_to_start();
            }
            EditAction::KillWordForward => {
                self.editor.kill_word_forward();
            }
            EditAction::KillWordBackward => {
                self.editor.kill_word_backward();
            }
            EditAction::Yank => {
                self.editor.yank();
            }
            EditAction::Bound(KeyBinding::DoKey(op)) => {
                return self.apply_dokey(op);
            }
            EditAction::Bound(KeyBinding::Macro(_body)) => {
                // Macro execution is handled by the event loop; we just
                // acknowledge here.
            }
        }
        None
    }

    fn apply_dokey(&mut self, op: DoKeyOp) -> Option<String> {
        let current = self.editor.text();
        match op {
            DoKeyOp::Newline => {
                let line = self.editor.take_line();
                self.history.record(&line);
                return Some(line);
            }
            DoKeyOp::RecallBackward => {
                if let Some(text) = self.history.recall(1, RecallMode::Exact, &current) {
                    let text = text.to_owned();
                    self.editor.set_text(&text);
                }
            }
            DoKeyOp::RecallForward => {
                if let Some(text) = self.history.recall(-1, RecallMode::Exact, &current) {
                    let text = text.to_owned();
                    self.editor.set_text(&text);
                }
            }
            DoKeyOp::RecallBeg => {
                if let Some(text) = self.history.recall(-1, RecallMode::Jump, &current) {
                    let text = text.to_owned();
                    self.editor.set_text(&text);
                }
            }
            DoKeyOp::RecallEnd => {
                if let Some(text) = self.history.recall(1, RecallMode::Jump, &current) {
                    let text = text.to_owned();
                    self.editor.set_text(&text);
                }
            }
            DoKeyOp::SearchBackward => {
                if let Some(text) = self.history.recall(1, RecallMode::Prefix, &current) {
                    let text = text.to_owned();
                    self.editor.set_text(&text);
                }
            }
            DoKeyOp::SearchForward => {
                if let Some(text) = self.history.recall(-1, RecallMode::Prefix, &current) {
                    let text = text.to_owned();
                    self.editor.set_text(&text);
                }
            }
            DoKeyOp::LiteralNext => {
                self.literal_next = true;
            }
            // Screen ops (Clear, Flush, Pause, Redraw, Refresh, SelFlush) are
            // handled by the event loop; nothing to do to the editor state.
            _ => {}
        }
        None
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a key sequence from a human-readable string.
///
/// Supports `^X` for control characters and `\eXX` / `\033` for escape.
/// Used primarily in tests.
pub fn key_sequence(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '^' => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    out.push(next.to_ascii_uppercase() as u8 - b'@');
                }
            }
            '\\' => match chars.next() {
                Some('e') | Some('E') => out.push(0x1b),
                Some('n') => out.push(b'\n'),
                Some('r') => out.push(b'\r'),
                Some('t') => out.push(b'\t'),
                Some(c) => out.push(c as u8),
                None => {}
            },
            _ => out.push(ch as u8),
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── DoKeyOp ───────────────────────────────────────────────────────────────

    #[test]
    fn dokey_all_have_names() {
        for &op in DoKeyOp::ALL {
            assert!(!op.name().is_empty());
        }
    }

    #[test]
    fn dokey_from_name_round_trip() {
        for &op in DoKeyOp::ALL {
            assert_eq!(DoKeyOp::from_name(op.name()), Some(op));
        }
    }

    #[test]
    fn dokey_from_name_case_insensitive() {
        assert_eq!(DoKeyOp::from_name("newline"), Some(DoKeyOp::Newline));
        assert_eq!(DoKeyOp::from_name("RECALLB"), Some(DoKeyOp::RecallBackward));
    }

    #[test]
    fn dokey_from_name_unknown_returns_none() {
        assert_eq!(DoKeyOp::from_name("XYZZY"), None);
    }

    // ── Keymap ────────────────────────────────────────────────────────────────

    #[test]
    fn bind_and_lookup() {
        let mut km = Keymap::new();
        km.bind(vec![0x01], KeyBinding::DoKey(DoKeyOp::Refresh));
        assert_eq!(
            km.lookup(&[0x01]),
            Some(&KeyBinding::DoKey(DoKeyOp::Refresh))
        );
    }

    #[test]
    fn unbind_removes_binding() {
        let mut km = Keymap::new();
        km.bind(vec![0x01], KeyBinding::DoKey(DoKeyOp::Refresh));
        km.unbind(&[0x01]);
        assert!(km.lookup(&[0x01]).is_none());
    }

    #[test]
    fn default_bindings_have_ctrl_m_newline() {
        let km = Keymap::new().with_defaults();
        assert_eq!(
            km.lookup(&[0x0D]),
            Some(&KeyBinding::DoKey(DoKeyOp::Newline))
        );
    }

    #[test]
    fn default_bindings_have_up_arrow() {
        let km = Keymap::new().with_defaults();
        let up = b"\x1b[A";
        assert_eq!(
            km.lookup(up),
            Some(&KeyBinding::DoKey(DoKeyOp::RecallBackward))
        );
    }

    // ── key_sequence helper ───────────────────────────────────────────────────

    #[test]
    fn key_sequence_ctrl() {
        assert_eq!(key_sequence("^A"), vec![0x01]);
        assert_eq!(key_sequence("^M"), vec![0x0D]);
    }

    #[test]
    fn key_sequence_escape() {
        assert_eq!(key_sequence("\\e[A"), b"\x1b[A".to_vec());
    }

    // ── InputProcessor ────────────────────────────────────────────────────────

    #[test]
    fn insert_and_newline() {
        let mut ip = InputProcessor::new(100);
        ip.apply(EditAction::InsertStr("hello".into()));
        let submitted = ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::Newline)));
        assert_eq!(submitted, Some("hello".to_owned()));
        assert!(ip.editor.is_empty());
    }

    #[test]
    fn recall_backward_after_submit() {
        let mut ip = InputProcessor::new(100);
        ip.apply(EditAction::InsertStr("first".into()));
        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::Newline)));

        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::RecallBackward)));
        assert_eq!(ip.editor.text(), "first");
    }

    #[test]
    fn recall_forward_returns_to_live() {
        let mut ip = InputProcessor::new(100);
        ip.apply(EditAction::InsertStr("first".into()));
        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::Newline)));
        ip.apply(EditAction::InsertStr("live".into()));

        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::RecallBackward)));
        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::RecallForward)));
        assert_eq!(ip.editor.text(), "live");
    }

    #[test]
    fn search_backward_prefix() {
        let mut ip = InputProcessor::new(100);
        for cmd in ["go north", "look", "go south"] {
            ip.apply(EditAction::InsertStr(cmd.into()));
            ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::Newline)));
        }
        // Type "go" then search backward.
        ip.apply(EditAction::InsertStr("go".into()));
        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::SearchBackward)));
        assert_eq!(ip.editor.text(), "go south");
    }

    #[test]
    fn literal_next_sets_flag() {
        let mut ip = InputProcessor::new(100);
        ip.apply(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::LiteralNext)));
        assert!(ip.literal_next);
    }
}
