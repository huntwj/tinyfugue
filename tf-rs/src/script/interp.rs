//! TF script interpreter.
//!
//! The [`Interpreter`] holds the variable stack and world state and
//! executes parsed [`Stmt`] trees.  It implements [`EvalContext`] so the
//! expression evaluator can call back into it for variable lookups and
//! function calls.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::hook::{Hook, HookSet};
use crate::macros::Macro;
use crate::pattern::{MatchMode, Pattern};
use super::{
    builtins::call_builtin,
    expand::expand,
    expr::{eval_str, EvalContext},
    stmt::{parse_script, Stmt},
    value::Value,
};

// ── ScriptAction ──────────────────────────────────────────────────────────────

/// A side-effect queued by the interpreter that requires event-loop involvement.
///
/// The interpreter is synchronous; these actions are drained by [`EventLoop`]
/// after each [`Interpreter::exec_script`] call (or, for `/load`/`/eval`,
/// processed inline by the interpreter's file loader).
#[derive(Debug, Clone)]
pub enum ScriptAction {
    /// Send text to a world (`None` = active world).
    SendToWorld { text: String, world: Option<String>, no_newline: bool },
    /// Open a connection to a named world (or the default world if empty).
    Connect { name: String },
    /// Close a connection.
    Disconnect { name: String },
    /// Add / update a world definition.
    AddWorld(crate::world::World),
    /// Switch the active world.
    SwitchWorld { name: String },
    /// Define (or redefine) a macro in the MacroStore.
    DefMacro(Macro),
    /// Terminate the event loop.
    Quit,

    // ── Process scheduling ─────────────────────────────────────────────────
    /// Schedule a `/repeat` process.
    AddRepeat { interval_ms: u64, count: Option<u32>, body: String, world: Option<String> },
    /// Schedule a `/quote 'file` process.
    AddQuoteFile { interval_ms: u64, path: String, world: Option<String> },
    /// Schedule a `/quote !cmd` process.
    AddQuoteShell { interval_ms: u64, command: String, world: Option<String> },
    /// Send all lines of a file immediately (`/quote -S 'file`).
    QuoteFileSync { path: String, world: Option<String> },
    /// Run a shell command and send all output lines immediately (`/quote -S !cmd`).
    QuoteShellSync { command: String, world: Option<String> },

    // ── Macro / binding management ─────────────────────────────────────────
    /// Remove a named macro (mirrors `/undef`).
    UndefMacro(String),
    /// Remove a key binding by sequence string (mirrors `/unbind`).
    UnbindKey(String),

    // ── Introspection ──────────────────────────────────────────────────────
    /// Display all defined worlds.
    ListWorlds,
    /// Display macros (optionally filtered by name prefix).
    ListMacros { filter: Option<String> },

    // ── Session logging ────────────────────────────────────────────────────
    /// Open a log file (mirrors `/log path`).
    StartLog(String),
    /// Close the current log file (mirrors `/nolog`).
    StopLog,

    // ── Persistence ────────────────────────────────────────────────────────
    /// Write world definitions to a file (or stdout) (`/saveworld [file]`).
    /// `name` filters to a single world when `Some`; `None` saves all worlds.
    SaveWorlds { path: Option<String>, name: Option<String> },

    // ── Miscellaneous ──────────────────────────────────────────────────────
    /// Ring the terminal bell (`/beep`).
    Bell,
    /// Pre-fill the input buffer with `text` (`/input text`).
    SetInput(String),
    /// Apply a built-in key operation to the input editor (`/dokey op`).
    DoKey(crate::keybind::DoKeyOp),
    /// Set the status bar format string (`/status [format]`).
    /// Tokens: `%world` (active world), `%T` (HH:MM), `%t` (HH:MM:SS).
    SetStatus(String),
    /// Delete macros whose name matches `pattern`, or all anonymous macros
    /// when `pattern` is `None` (`/purge [pattern]`).
    PurgeMacros(Option<String>),
    /// Run `sh -c <cmd>` and display stdout/stderr on the TF screen (`/sh cmd`).
    ShellCmd(String),
    /// Spawn an interactive shell, temporarily leaving raw mode (`/sh` with no args).
    ShellInteractive,
    /// Display the last `n` input history entries on screen (`/recall [n]`).
    /// `None` means show all.
    Recall(Option<usize>),
    /// Write all macros to a file (or stdout) in `/def` form (`/save [file]`).
    SaveMacros { path: Option<String> },
    /// Remove a world definition by name (`/unworld name`).
    UnWorld(String),
    /// Set the scrollback buffer depth (`/histsize n`).
    SetHistSize(usize),
    /// List all active scheduled processes (`/ps`).
    ListProcesses,
    /// Kill a scheduled process by ID (`/kill id`).
    KillProcess(u32),
    /// Inject a line as if received from the server (`fake_recv([world,] line)`).
    FakeRecv { world: Option<String>, line: String },
    /// Print a local diagnostic/info line to the TF screen (not sent to server,
    /// not run through triggers).  Corresponds to C TF's `tf_wprintf`.
    LocalLine(String),
    /// Remove all macros in the `MacroStore` whose name matches `pattern` (`/undefn pattern`).
    UndefMacrosMatching(String),
    /// Scroll the pager by `n` lines (negative = back, positive = forward) (`morescroll(n)`).
    MoreScroll(i64),
    /// Delete `n` characters at the cursor position in the input editor (`kbdel(n)`).
    KbDel(usize),
    /// Move the input-editor cursor to char position `pos` (`kbgoto(pos)`).
    KbGoto(usize),

    // ── Status bar field management ────────────────────────────────────────
    /// Add fields to the status bar (`/status_add [-c] field_spec…`).
    /// `clear` = wipe existing fields first; `raw` = unexpanded spec string.
    StatusAdd { clear: bool, raw: String },
    /// Remove a named field from the status bar (`/status_rm name`).
    StatusRm(String),
    /// Edit the width/label of an existing field (`/status_edit name spec`).
    StatusEdit { name: String, raw: String },
    /// Remove all status bar fields (`/status_clear`).
    StatusClear,
    /// Add a line to the input history (`/recordline line`).
    RecordLine(String),
    /// Set the watchdog interval in seconds (0 = disable) (`/watchdog [n]`).
    SetWatchdog(u64),
    /// Set which world the watchdog monitors (`/watchname [name]`; empty = active world).
    SetWatchName(String),

    /// Suspend the process (leave raw mode, SIGSTOP, resume, repaint) (`/suspend`).
    Suspend,
    /// Open the current input buffer in `$EDITOR`, then re-insert the result (`/edit`).
    EditInput,
    /// Send an option-102 subnegotiation to a world (`option102([world,] data)`).
    Option102 { data: Vec<u8>, world: Option<String> },

    // ── Lua scripting (requires the `lua` Cargo feature) ──────────────────
    /// Load and execute a Lua source file (`/loadlua path`).
    #[cfg(feature = "lua")]
    LuaLoad(String),
    /// Call a named Lua function with string arguments (`/calllua func args…`).
    #[cfg(feature = "lua")]
    LuaCall { func: String, args: Vec<String> },
    /// Drop the Lua interpreter (`/purgelua`).
    #[cfg(feature = "lua")]
    LuaPurge,

    // ── Python scripting (requires the `python` Cargo feature) ────────────
    /// Execute Python statements (`/python code`).
    #[cfg(feature = "python")]
    PythonExec(String),
    /// Call a named Python function with one string argument (`/callpython func arg`).
    #[cfg(feature = "python")]
    PythonCall { func: String, arg: String },
    /// Import or reload a Python module (`/loadpython module`).
    #[cfg(feature = "python")]
    PythonLoad(String),
    /// Destroy the Python interpreter (`/killpython`).
    #[cfg(feature = "python")]
    PythonKill,
}

// ── Restriction level ─────────────────────────────────────────────────────────

/// Command-restriction level set by `/restrict`.
///
/// Can only be raised, not lowered.  Mirrors the C `restriction` global.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum RestrictionLevel {
    #[default]
    None  = 0,
    /// No shell access (`/sh`, `/quote !cmd`).
    Shell = 1,
    /// + no file I/O (`/load`, `/log`, `/nolog`, `/save`).
    File  = 2,
    /// + no network commands (`/connect`, `/disconnect`).
    World = 3,
}

// ── File loader callback ──────────────────────────────────────────────────────

/// A callback that resolves a path string (after variable expansion) to the
/// file's contents.  The loader is responsible for tilde expansion and file
/// system access.
pub type FileLoader = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

// ── TF file descriptors ───────────────────────────────────────────────────────

/// An open file managed by `tfopen` / `tfread` / `tfwrite` / `tfclose`.
enum TfFile {
    Reader(std::io::BufReader<std::fs::File>),
    Writer(std::fs::File),
    Appender(std::fs::File),
}

// ── ControlFlow ───────────────────────────────────────────────────────────────

/// Non-error control-flow signals that can unwind the call stack.
#[derive(Debug)]
pub enum ControlFlow {
    Break,
    Return(Value),
}

// ── Variable scope frame ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Frame {
    locals: HashMap<String, Value>,
    params: Vec<String>,
    cmd_name: String,
}

// ── CachedMacro ───────────────────────────────────────────────────────────────

/// A user-defined macro whose body is parsed lazily on first invocation and
/// cached thereafter, avoiding O(body_len) work on every trigger fire.
struct CachedMacro {
    body: String,
    parsed: Option<Vec<Stmt>>,
}

impl CachedMacro {
    fn new(body: String) -> Self {
        Self { body, parsed: None }
    }

    /// Return the parsed statement list, compiling on first call.
    fn stmts(&mut self) -> Result<&[Stmt], String> {
        if self.parsed.is_none() {
            self.parsed = Some(parse_script(&self.body)?);
        }
        Ok(self.parsed.as_deref().unwrap())
    }
}

// ── Interpreter ───────────────────────────────────────────────────────────────

/// The TF script interpreter.
pub struct Interpreter {
    /// Global variable store.
    globals: HashMap<String, Value>,
    /// Local variable stack (innermost frame last).
    frames: Vec<Frame>,
    /// User-defined macros: name → body + parse cache.
    macros: HashMap<String, CachedMacro>,
    /// Lines of output produced by `/echo`.
    pub output: Vec<String>,
    /// Side-effects queued for the event loop.
    pub actions: Vec<ScriptAction>,
    /// Optional callback for `/load` and `/eval /load …`.
    pub file_loader: Option<FileLoader>,
    /// Open file descriptors for `tfopen`/`tfread`/`tfwrite`/`tfclose`.
    tf_files: HashMap<i64, TfFile>,
    /// Counter for the next fd to allocate (TF uses 1-based fds).
    next_tf_fd: i64,
    /// Snapshot of world definitions, synced by the event loop in `update_status()`.
    /// Maps world name → `[host, port, type, character, mfile]` (all `Option<String>`).
    pub worlds_snapshot: HashMap<String, [Option<String>; 5]>,
    /// Names of macros added via `/def` (via `ScriptAction::DefMacro`), used by `ismacro()`.
    pub macro_names: HashSet<String>,
    /// Current restriction level (set by `/restrict`; can only increase).
    pub restriction: RestrictionLevel,
    /// When `true`, `/set` and `/let` at the top level (no macro frame) do NOT
    /// expand `%name` variables — mirrors C TF's `SUB_KEYWORD` file-load mode.
    /// Set by the `/load` handler and `load_script_source` for the duration of
    /// file loading; `false` in interactive/test mode so expansion works normally.
    pub file_load_mode: bool,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            globals: HashMap::new(),
            frames: Vec::new(),
            macros: HashMap::new(),
            output: Vec::new(),
            actions: Vec::new(),
            file_loader: None,
            tf_files: HashMap::new(),
            next_tf_fd: 1,
            worlds_snapshot: HashMap::new(),
            macro_names: HashSet::new(),
            restriction: RestrictionLevel::None,
            file_load_mode: false,
        }
    }

    /// Define a user macro (name → TF script source).
    pub fn define_macro(&mut self, name: impl Into<String>, body: impl Into<String>) {
        self.macros.insert(name.into(), CachedMacro::new(body.into()));
    }

    /// Remove a user macro by name.  Returns `true` if it existed.
    pub fn undef_macro(&mut self, name: &str) -> bool {
        self.macros.remove(name).is_some()
    }

    /// Set a global variable.
    pub fn set_global_var(&mut self, name: impl Into<String>, value: Value) {
        self.globals.insert(name.into(), value);
    }

    /// Get a global variable.
    pub fn get_global_var(&self, name: &str) -> Option<&Value> {
        self.globals.get(name)
    }

    /// Drain and return all queued [`ScriptAction`]s.
    pub fn take_actions(&mut self) -> Vec<ScriptAction> {
        std::mem::take(&mut self.actions)
    }

    // ── Execution ─────────────────────────────────────────────────────────────

    /// Execute a TF script string.
    pub fn exec_script(&mut self, src: &str) -> Result<Value, String> {
        let stmts = parse_script(src).map_err(|e| format!("parse error: {e}"))?;
        match self.exec_block(&stmts)? {
            Some(ControlFlow::Return(v)) => Ok(v),
            Some(ControlFlow::Break) => Ok(Value::default()),
            None => Ok(Value::default()),
        }
    }

    /// Execute a pre-parsed block of statements.
    pub fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Option<ControlFlow>, String> {
        for stmt in stmts {
            if let Some(cf) = self.exec_stmt(stmt)? {
                return Ok(Some(cf));
            }
        }
        Ok(None)
    }

    /// Execute a single statement.
    pub fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Option<ControlFlow>, String> {
        match stmt {
            Stmt::Raw(text) => {
                let expanded = expand(text, self)?;
                self.output.push(expanded);
                Ok(None)
            }

            Stmt::Echo { text, newline } => {
                let expanded = expand(text, self)?;
                if *newline {
                    self.output.push(expanded);
                } else if let Some(last) = self.output.last_mut() {
                    last.push_str(&expanded);
                } else {
                    self.output.push(expanded);
                }
                Ok(None)
            }

            Stmt::Send { text } => {
                let expanded = expand(text, self)?;
                self.actions.push(ScriptAction::SendToWorld {
                    text: expanded,
                    world: None,
                    no_newline: false,
                });
                Ok(None)
            }

            Stmt::Set { name, value } => {
                // In C TF, /set values at file-load level (SUB_KEYWORD) are stored
                // literally — %H in "%H:%M" is NOT expanded to the (undefined) variable H.
                // We replicate this: skip expansion when loading a file AND not inside a
                // macro frame (top-level file statement).  Inside a macro body (frames
                // non-empty) full expansion applies regardless of file_load_mode.
                let expanded = if self.file_load_mode && self.frames.is_empty() {
                    value.clone()
                } else {
                    expand(value, self)?
                };
                let val = try_parse_number(&expanded);
                self.set_global(name, val);
                Ok(None)
            }

            Stmt::Let { name, value } => {
                let expanded = if self.file_load_mode && self.frames.is_empty() {
                    value.clone()
                } else {
                    expand(value, self)?
                };
                let val = try_parse_number(&expanded);
                self.set_local(name, val);
                Ok(None)
            }

            Stmt::Unset { name } => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.locals.remove(name);
                }
                self.globals.remove(name);
                Ok(None)
            }

            Stmt::Expr { src } => {
                let expanded = expand(src, self)?;
                eval_str(&expanded, self)?;
                Ok(None)
            }

            Stmt::Return { value } => {
                let v = match value {
                    None => Value::default(),
                    Some(src) => {
                        let expanded = expand(src, self)?;
                        eval_str(&expanded, self)?
                    }
                };
                Ok(Some(ControlFlow::Return(v)))
            }

            Stmt::Break => Ok(Some(ControlFlow::Break)),

            Stmt::If { cond, then_block, else_block } => {
                let expanded = expand(cond, self)?;
                let val = self.eval_condition(&expanded)?;
                let block = if val.as_bool() { then_block } else { else_block };
                self.exec_block(block)
            }

            Stmt::While { cond, body } => {
                loop {
                    let expanded = expand(cond, self)?;
                    let val = self.eval_condition(&expanded)?;
                    if !val.as_bool() {
                        break;
                    }
                    match self.exec_block(body)? {
                        Some(ControlFlow::Break) => break,
                        Some(cf @ ControlFlow::Return(_)) => return Ok(Some(cf)),
                        None => {}
                    }
                }
                Ok(None)
            }

            Stmt::For { var, start, end, body } => {
                // TF range loop: iterate var from start to end (inclusive).
                let start_str = expand(start, self)?;
                let end_str = expand(end, self)?;
                let start_val: i64 = start_str
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid /for start value: {start_str}"))?;
                let end_val: i64 = end_str
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid /for end value: {end_str}"))?;
                for i in start_val..=end_val {
                    self.set_local(var, Value::Int(i));
                    match self.exec_block(body)? {
                        Some(ControlFlow::Break) => break,
                        Some(cf @ ControlFlow::Return(_)) => return Ok(Some(cf)),
                        None => {}
                    }
                }
                Ok(None)
            }

            Stmt::AddWorld { args } => {
                match crate::config::parse_world_from_tokens(args) {
                    Ok(world) => self.actions.push(ScriptAction::AddWorld(world)),
                    Err(e) => self.output.push(format!("% {e}")),
                }
                Ok(None)
            }

            Stmt::Command { name, args } => {
                // Try user-defined macro first.
                if self.macros.contains_key(name.as_str()) {
                    let stmts = self
                        .macros
                        .get_mut(name.as_str())
                        .unwrap()
                        .stmts()?
                        .to_vec();
                    let expanded_args = expand(args, self)?;
                    let params: Vec<String> =
                        expanded_args.split_whitespace().map(str::to_owned).collect();
                    return self.invoke_macro(stmts, name, params);
                }

                // Built-in command dispatch.
                self.exec_builtin(name, args)
            }
        }
    }

    // ── Condition evaluation ───────────────────────────────────────────────────

    /// Evaluate a condition string: if it starts with `/`, run it as a command
    /// and use the return value; otherwise evaluate it as an expression.
    fn eval_condition(&mut self, expanded: &str) -> Result<Value, String> {
        let trimmed = expanded.trim();
        if trimmed.starts_with('/') {
            let stmts = parse_script(trimmed)
                .map_err(|e| format!("condition parse error: {e}"))?;
            match self.exec_block(&stmts)? {
                Some(ControlFlow::Return(v)) => Ok(v),
                _ => Ok(Value::Int(0)),
            }
        } else {
            eval_str(trimmed, self)
        }
    }

    // ── Built-in command dispatch ──────────────────────────────────────────────

    fn exec_builtin(&mut self, name: &str, args: &str) -> Result<Option<ControlFlow>, String> {
        // Strip the TF `@` prefix used for expression-returning commands like
        // `/@test`, `/@eval`, `/@list`.  The `@` is a capture hint for the C
        // interpreter; we simply treat them as their base command.
        let name = name.strip_prefix('@').unwrap_or(name);
        match name {
            // ── Lifecycle ──────────────────────────────────────────────────────
            "quit" | "exit" => {
                self.actions.push(ScriptAction::Quit);
                Ok(Some(ControlFlow::Return(Value::default())))
            }

            // ── Macro definition ───────────────────────────────────────────────
            "def" | "trigger" | "hook" | "bind" => {
                // /def [-flags…] name = body
                let mac = parse_def(args);
                if let Some(name) = &mac.name {
                    if let Some(body) = &mac.body {
                        self.macros.insert(name.clone(), CachedMacro::new(body.clone()));
                    }
                }
                self.actions.push(ScriptAction::DefMacro(mac));
                Ok(None)
            }

            // ── File loading ───────────────────────────────────────────────────
            "load" => {
                // /load [-q] [-L <dir>] <file>
                if self.restriction >= RestrictionLevel::File {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                // Expand args FIRST (e.g. %{-L} %{L} in the `require` macro),
                // THEN parse flags so that -q/-L embedded via expansion are seen.
                let expanded = expand(args, self)?;
                let (quiet, raw_path) = parse_load_flags(&expanded);
                let raw_path = raw_path.trim().to_owned();

                let loader = self.file_loader.clone();
                if let Some(loader) = loader {
                    // For bare filenames (no leading /, ./, ~/) try TFLIBDIR first,
                    // then fall back to the literal path — mirrors C TF behaviour.
                    let is_relative = !raw_path.is_empty()
                        && !raw_path.starts_with('/')
                        && !raw_path.starts_with('.')
                        && !raw_path.starts_with('~');

                    let paths: Vec<String> = if is_relative {
                        let mut v = Vec::new();
                        if let Some(libdir) =
                            self.get_var("TFLIBDIR").map(|v| v.to_string())
                        {
                            v.push(format!("{libdir}/{raw_path}"));
                        }
                        v.push(raw_path.clone());
                        v
                    } else {
                        vec![raw_path.clone()]
                    };

                    let mut loaded = false;
                    let mut last_err = String::new();
                    for path in &paths {
                        match loader(path) {
                            Ok(src) => {
                                if !quiet {
                                    self.output.push(format!("% Loading commands from {path}."));
                                }
                                let stmts = parse_script(&src)
                                    .map_err(|e| format!("{path}: parse error: {e}"))?;
                                let old_flm = self.file_load_mode;
                                self.file_load_mode = true;
                                let r = self.exec_block(&stmts);
                                self.file_load_mode = old_flm;
                                r?;
                                loaded = true;
                                break;
                            }
                            Err(e) => last_err = e,
                        }
                    }
                    if !loaded && !quiet {
                        self.output.push(format!("% {last_err}"));
                    }
                }
                Ok(None)
            }

            // ── Expression / command evaluation ───────────────────────────────
            "eval" => {
                // /eval <command-or-expression>
                let expanded = expand(args, self)?;
                let trimmed = expanded.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                let stmts =
                    parse_script(trimmed).map_err(|e| format!("/eval: parse error: {e}"))?;
                self.exec_block(&stmts)
            }

            // ── World / connection management ──────────────────────────────────
            "addworld" => {
                let tokens: Vec<String> =
                    tokenise_addworld(&expand(args, self)?);
                match crate::config::parse_world_from_tokens(&tokens) {
                    Ok(world) => self.actions.push(ScriptAction::AddWorld(world)),
                    Err(e) => self.output.push(format!("% {e}")),
                }
                Ok(None)
            }

            "connect" | "fg" => {
                if self.restriction >= RestrictionLevel::World {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::Connect { name: expanded.trim().to_owned() });
                Ok(None)
            }

            "disconnect" | "dc" => {
                if self.restriction >= RestrictionLevel::World {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::Disconnect {
                    name: expanded.trim().to_owned(),
                });
                Ok(None)
            }

            "world" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::SwitchWorld {
                    name: expanded.trim().to_owned(),
                });
                Ok(None)
            }

            // ── Echo / output ──────────────────────────────────────────────────
            "echo" => {
                // Strip known flags: -n (no newline), -e (error stream),
                // -p (prompt), -A (activity), -w world, -r (raw/no-expand),
                // -s (silent).  Unrecognised flags are left in the text.
                let mut rest = args;
                let mut no_nl  = false;
                let mut silent = false;
                loop {
                    rest = rest.trim_start();
                    if rest.starts_with("-n") && rest[2..].starts_with(|c: char| c.is_whitespace() || c == '-') {
                        no_nl = true;
                        rest = &rest[2..];
                    } else if rest.starts_with("-s") && rest[2..].starts_with(|c: char| c.is_whitespace() || c == '-') {
                        silent = true;
                        rest = &rest[2..];
                    } else if rest.starts_with("-e") && rest[2..].starts_with(|c: char| c.is_whitespace() || c == '-') {
                        rest = &rest[2..]; // error stream → normal output
                    } else if rest.starts_with("-p") && rest[2..].starts_with(|c: char| c.is_whitespace() || c == '-') {
                        rest = &rest[2..]; // prompt → normal output
                    } else if rest.starts_with("-A") && rest[2..].starts_with(|c: char| c.is_whitespace() || c == '-') {
                        rest = &rest[2..]; // activity → ignored
                    } else if rest.starts_with("-r") && rest[2..].starts_with(|c: char| c.is_whitespace() || c == '-') {
                        rest = &rest[2..]; // raw — TODO: skip expansion
                    } else if let Some(r) = rest.strip_prefix("-w") {
                        // -w worldname: skip the world name token
                        let r = r.trim_start();
                        rest = r.split_once(char::is_whitespace).map(|x| x.1).unwrap_or("");
                    } else {
                        break;
                    }
                }
                if silent {
                    return Ok(None);
                }
                let expanded = expand(rest, self)?;
                if no_nl {
                    if let Some(last) = self.output.last_mut() {
                        last.push_str(&expanded);
                    } else {
                        self.output.push(expanded);
                    }
                } else {
                    self.output.push(expanded);
                }
                Ok(None)
            }

            // ── Macro removal ─────────────────────────────────────────────────
            "undef" => {
                let expanded = expand(args, self)?;
                let n = expanded.trim().to_owned();
                self.macros.remove(&n);
                self.actions.push(ScriptAction::UndefMacro(n));
                Ok(None)
            }

            // ── Key binding removal ────────────────────────────────────────────
            "unbind" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::UnbindKey(expanded.trim().to_owned()));
                Ok(None)
            }

            // ── Process scheduling ─────────────────────────────────────────────
            "trigpc" => {
                // /trigpc chance body — execute body with `chance` percent probability.
                let expanded = expand(args, self)?;
                let (chance_str, body) = expanded.trim()
                    .split_once(char::is_whitespace)
                    .map(|(a, b)| (a, b.trim_start()))
                    .unwrap_or((expanded.trim(), ""));
                let chance: u64 = chance_str.parse().unwrap_or(0).clamp(0, 100);
                if chance > 0 && trigpc_roll() < chance {
                    self.exec_script(body)?;
                }
                Ok(None)
            }

            "repeat" => {
                let expanded = expand(args, self)?;
                let (interval_ms, count, body, world) = parse_repeat_args(expanded.trim());
                self.actions.push(ScriptAction::AddRepeat { interval_ms, count, body, world });
                Ok(None)
            }

            "quote" => {
                let expanded = expand(args, self)?;
                let trimmed = expanded.trim();
                // Format: /quote [-S] [/interval] 'file   or   /quote [-S] [/interval] !cmd
                // -S means synchronous: send all lines immediately without scheduling.
                let (sync, after_flag) = if let Some(r) = trimmed.strip_prefix("-S") {
                    (true, r.trim_start())
                } else {
                    (false, trimmed)
                };
                let (interval_ms, rest) = parse_quote_interval(after_flag);
                if let Some(path) = rest.strip_prefix('\'') {
                    if sync {
                        self.actions.push(ScriptAction::QuoteFileSync {
                            path: path.to_owned(),
                            world: None,
                        });
                    } else {
                        self.actions.push(ScriptAction::AddQuoteFile {
                            interval_ms,
                            path: path.to_owned(),
                            world: None,
                        });
                    }
                } else if let Some(cmd) = rest.strip_prefix('!') {
                    if self.restriction >= RestrictionLevel::Shell {
                        self.output.push("% restricted".to_owned());
                    } else if sync {
                        self.actions.push(ScriptAction::QuoteShellSync {
                            command: cmd.to_owned(),
                            world: None,
                        });
                    } else {
                        self.actions.push(ScriptAction::AddQuoteShell {
                            interval_ms,
                            command: cmd.to_owned(),
                            world: None,
                        });
                    }
                } else {
                    self.output.push("% /quote: expected 'file or !command".to_owned());
                }
                Ok(None)
            }

            // ── Introspection ──────────────────────────────────────────────────
            "listworlds" | "worlds" | "listsockets" => {
                self.actions.push(ScriptAction::ListWorlds);
                Ok(None)
            }

            // ── Status bar field management ────────────────────────────────────
            "status_add" | "status-add" => {
                let expanded = expand(args, self)?;
                let mut rest = expanded.trim();
                let mut clear = false;
                loop {
                    rest = rest.trim_start();
                    if let Some(r) = rest.strip_prefix("-c") {
                        clear = true;
                        rest = r;
                    } else if let Some(r) = rest.strip_prefix("-A")
                        .or_else(|| rest.strip_prefix("-B"))
                        .or_else(|| rest.strip_prefix("-x"))
                    {
                        rest = r;
                    } else {
                        break;
                    }
                }
                // Skip bare `-` separator tokens.
                let raw = rest.split_whitespace()
                    .filter(|t| *t != "-")
                    .collect::<Vec<_>>()
                    .join(" ");
                self.actions.push(ScriptAction::StatusAdd { clear, raw });
                Ok(None)
            }

            "status_rm" | "status-rm" => {
                let expanded = expand(args, self)?;
                let name = expanded.trim().trim_start_matches('@').to_owned();
                self.actions.push(ScriptAction::StatusRm(name));
                Ok(None)
            }

            "status_edit" | "status-edit" => {
                let expanded = expand(args, self)?;
                let trimmed = expanded.trim();
                if let Some((name_tok, new_spec)) = trimmed.split_once(char::is_whitespace) {
                    let name = name_tok.trim_start_matches('@').to_owned();
                    let raw  = new_spec.trim().to_owned();
                    self.actions.push(ScriptAction::StatusEdit { name, raw });
                }
                Ok(None)
            }

            "status_clear" | "status-clear" => {
                self.actions.push(ScriptAction::StatusClear);
                Ok(None)
            }

            "shift" => {
                // /shift [n] — drop the first n positional params (default 1).
                let expanded = expand(args, self)?;
                let n: usize = expanded.trim().parse().unwrap_or(1).max(1);
                if let Some(frame) = self.frames.last_mut() {
                    let skip = n.min(frame.params.len());
                    frame.params.drain(..skip);
                }
                Ok(None)
            }

            "undefn" => {
                // /undefn pattern — bulk-remove macros whose name matches pattern.
                let expanded = expand(args, self)?;
                let pat = expanded.trim().to_owned();
                self.actions.push(ScriptAction::UndefMacrosMatching(pat));
                Ok(None)
            }

            "listvar" => {
                let expanded = expand(args, self)?;
                let pat = expanded.trim().to_owned();
                let mut vars: Vec<(&String, &Value)> = self.globals.iter()
                    .filter(|(k, _)| pat.is_empty() || k.contains(pat.as_str()))
                    .collect();
                vars.sort_by_key(|(k, _)| k.as_str());
                if vars.is_empty() {
                    self.output.push("% No variables defined.".to_owned());
                } else {
                    for (k, v) in vars {
                        self.output.push(format!("% {k}={v}"));
                    }
                }
                Ok(None)
            }

            "list" | "listdefs" => {
                let filter = {
                    let expanded = expand(args, self)?;
                    let s = expanded.trim().to_owned();
                    if s.is_empty() { None } else { Some(s) }
                };
                self.actions.push(ScriptAction::ListMacros { filter });
                Ok(None)
            }

            // ── Logging ───────────────────────────────────────────────────────
            "log" => {
                if self.restriction >= RestrictionLevel::File {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                let expanded = expand(args, self)?;
                let path = expanded.trim();
                if path.is_empty() {
                    self.output.push("% /log: missing filename".to_owned());
                } else {
                    self.actions.push(ScriptAction::StartLog(path.to_owned()));
                }
                Ok(None)
            }

            "nolog" => {
                if self.restriction >= RestrictionLevel::File {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                self.actions.push(ScriptAction::StopLog);
                Ok(None)
            }

            // ── Miscellaneous ─────────────────────────────────────────────────
            "unworld" => {
                let expanded = expand(args, self)?;
                let name = expanded.trim().to_owned();
                if name.is_empty() {
                    self.output.push("% /unworld: world name required".to_owned());
                } else {
                    self.actions.push(ScriptAction::UnWorld(name));
                }
                Ok(None)
            }

            "histsize" => {
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                match s.parse::<usize>() {
                    Ok(n) if n > 0 => {
                        self.actions.push(ScriptAction::SetHistSize(n));
                    }
                    _ => {
                        self.output.push(format!("% /histsize: expected positive integer, got '{s}'"));
                    }
                }
                Ok(None)
            }

            "save" => {
                if self.restriction >= RestrictionLevel::File {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                let path = if s.is_empty() { None } else { Some(s.to_owned()) };
                self.actions.push(ScriptAction::SaveMacros { path });
                Ok(None)
            }

            "saveworld" => {
                // /saveworld [-w <world>] [<file>]
                // Flags: -w <name> restricts to one world.
                let expanded = expand(args, self)?;
                let mut rest = expanded.trim();
                let mut world_name: Option<String> = None;

                // Consume optional -w <name> flag.
                if rest.starts_with("-w") {
                    rest = rest[2..].trim_start();
                    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                    world_name = Some(rest[..end].to_owned());
                    rest = rest[end..].trim_start();
                }

                let path = if rest.is_empty() { None } else { Some(rest.to_owned()) };
                self.actions.push(ScriptAction::SaveWorlds { path, name: world_name });
                Ok(None)
            }

            // ── Variable management ───────────────────────────────────────────
            "unset" => {
                // /unset varname  — remove a global variable.
                let expanded = expand(args, self)?;
                for name in expanded.split_whitespace() {
                    self.globals.remove(name);
                }
                Ok(None)
            }

            // ── Display-mode stubs ────────────────────────────────────────────
            // The Rust binary always runs in visual (full-screen) mode.
            // /visual and /mode are accepted and silently succeed so that
            // config files that contain them don't produce errors.
            "visual" | "mode" | "redraw" | "localecho" => Ok(None),

            "features" => {
                // /features [name]
                //   • No arg: print the feature string.
                //   • With a name: echo "1" or "0" (function form via call_fn handles
                //     the return-value path; this is the command-line output path).
                use super::builtins::{tf_features_string, tf_has_feature};
                let arg = expand(args, self)?.trim().to_owned();
                if arg.is_empty() {
                    self.output.push(tf_features_string());
                } else {
                    let yn = tf_has_feature(&arg) as i64;
                    self.output.push(yn.to_string());
                }
                Ok(None)
            }

            "version" => {
                self.output.push(format!(
                    "% TinyFugue (tf) version {} (Rust rewrite).",
                    env!("CARGO_PKG_VERSION"),
                ));
                self.output.push(
                    "% Copyright (C) 1993-2007 Ken Keys.  \
                     Rust rewrite (C) 2024-2025 contributors."
                    .to_owned(),
                );
                self.output
                    .push("% Rust rewrite of TinyFugue.".to_owned());
                self.output.push(format!(
                    "% Built for {}.",
                    std::env::consts::OS,
                ));
                Ok(None)
            }

            "beep" => {
                self.actions.push(ScriptAction::Bell);
                Ok(None)
            }

            "sh" => {
                if self.restriction >= RestrictionLevel::Shell {
                    self.output.push("% restricted".to_owned());
                    return Ok(None);
                }
                let cmd = args.trim();
                if cmd.is_empty() {
                    self.actions.push(ScriptAction::ShellInteractive);
                } else {
                    self.actions.push(ScriptAction::ShellCmd(cmd.to_owned()));
                }
                Ok(None)
            }

            "ps" => {
                self.actions.push(ScriptAction::ListProcesses);
                Ok(None)
            }

            "kill" => {
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                match s.parse::<u32>() {
                    Ok(id) => {
                        self.actions.push(ScriptAction::KillProcess(id));
                    }
                    Err(_) => {
                        self.output.push(format!("% /kill: invalid process id '{s}'"));
                    }
                }
                Ok(None)
            }

            "recall" => {
                let n = {
                    let s = expand(args, self)?;
                    let t = s.trim();
                    if t.is_empty() {
                        None
                    } else {
                        match t.parse::<usize>() {
                            Ok(n) => Some(n),
                            Err(_) => {
                                self.output.push(format!("% /recall: invalid count '{t}'"));
                                return Ok(None);
                            }
                        }
                    }
                };
                self.actions.push(ScriptAction::Recall(n));
                Ok(None)
            }

            "lcd" | "cd" => {
                let expanded = expand(args, self)?;
                let raw = expanded.trim();
                let dir = if raw.is_empty() {
                    std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())
                } else if let Some(rest) = raw.strip_prefix('~') {
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
                    if rest.is_empty() {
                        home
                    } else {
                        format!("{home}{rest}")
                    }
                } else {
                    raw.to_owned()
                };
                match std::env::set_current_dir(&dir) {
                    Ok(()) => {
                        let cwd = std::env::current_dir()
                            .map(|p| p.display().to_string())
                            .unwrap_or(dir);
                        self.output.push(format!("% {cwd}"));
                    }
                    Err(e) => {
                        self.output.push(format!("% /lcd: {dir}: {e}"));
                    }
                }
                Ok(None)
            }

            "setenv" => {
                // /setenv NAME=value  or  /setenv NAME value
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                // SAFETY: TF is single-threaded from the user's perspective;
                // the Tokio runtime has no other threads reading env vars.
                unsafe {
                    if let Some((name, val)) = s.split_once('=') {
                        std::env::set_var(name.trim(), val);
                    } else if let Some((name, val)) = s.split_once(char::is_whitespace) {
                        std::env::set_var(name, val.trim_start());
                    }
                }
                Ok(None)
            }

            "export" => {
                // /export name  — copy TF variable `name` to the process environment.
                let expanded = expand(args, self)?;
                let vname = expanded.trim().to_owned();
                if let Some(val) = self.get_var(&vname) {
                    let sval = val.to_string();
                    // SAFETY: same single-threaded guarantee as /setenv above.
                    unsafe { std::env::set_var(&vname, &sval); }
                } else {
                    self.output.push(format!("% {vname} not defined."));
                }
                Ok(None)
            }

            "purge" => {
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                let pattern = if s.is_empty() { None } else { Some(s.to_owned()) };
                self.actions.push(ScriptAction::PurgeMacros(pattern));
                Ok(None)
            }

            "input" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::SetInput(expanded));
                Ok(None)
            }

            "recordline" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::RecordLine(expanded));
                Ok(None)
            }

            "watchdog" => {
                let expanded = expand(args, self)?;
                let secs: u64 = expanded.trim().parse().unwrap_or(0);
                self.actions.push(ScriptAction::SetWatchdog(secs));
                Ok(None)
            }

            "watchname" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::SetWatchName(expanded.trim().to_owned()));
                Ok(None)
            }

            "restrict" => {
                // /restrict [none|shell|file|world]
                // With no arg, prints current level.  Level can only increase.
                let expanded = expand(args, self)?;
                let s = expanded.trim().to_ascii_lowercase();
                if s.is_empty() {
                    let level_name = match self.restriction {
                        RestrictionLevel::None  => "none",
                        RestrictionLevel::Shell => "shell",
                        RestrictionLevel::File  => "file",
                        RestrictionLevel::World => "world",
                    };
                    self.output.push(format!("% restriction level: {level_name}"));
                } else {
                    let new_level = match s.as_str() {
                        "none"  => RestrictionLevel::None,
                        "shell" => RestrictionLevel::Shell,
                        "file"  => RestrictionLevel::File,
                        "world" => RestrictionLevel::World,
                        _ => {
                            self.output.push(format!("% /restrict: invalid level {s:?}"));
                            return Ok(None);
                        }
                    };
                    if new_level < self.restriction {
                        self.output.push("% Restriction level can not be lowered.".to_owned());
                    } else {
                        self.restriction = new_level;
                    }
                }
                Ok(None)
            }

            "suspend" => {
                self.actions.push(ScriptAction::Suspend);
                Ok(None)
            }

            // [C5] /edit — open current input buffer in $EDITOR, re-insert on exit.
            "edit" => {
                self.actions.push(ScriptAction::EditInput);
                Ok(None)
            }

            // [C1] /gag [pattern] — set global gag or create a gag trigger.
            "gag" => {
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                if s.is_empty() {
                    // No args: enable global gag.
                    self.set_global("gag", Value::Int(1));
                } else {
                    // With pattern: shorthand for /def -ag <pattern>.
                    let body = format!("/def -ag {s}");
                    if let Ok(stmts) = parse_script(&body) {
                        self.exec_block(&stmts)?;
                    }
                }
                Ok(None)
            }

            // [C2] /hilite [pattern] — set global hilite or create a hilite trigger.
            "hilite" => {
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                if s.is_empty() {
                    // No args: enable global hilite.
                    self.set_global("hilite", Value::Int(1));
                } else {
                    // With pattern: shorthand for /def -ah <pattern>.
                    let body = format!("/def -ah {s}");
                    if let Ok(stmts) = parse_script(&body) {
                        self.exec_block(&stmts)?;
                    }
                }
                Ok(None)
            }

            // [C3] /relimit — re-enable output limiting after it was hit.
            // [C3] /unlimit — remove the output limit entirely.
            // Both are stubs; real paged-output tracking not yet implemented.
            "relimit" => {
                self.set_global("more", Value::Int(1));
                Ok(None)
            }
            "unlimit" => {
                self.set_global("more", Value::Int(0));
                Ok(None)
            }

            // [C4] /core — dump debug state (stub).
            "core" => {
                self.output.push("% /core: debug dump not implemented in Rust build.".to_owned());
                Ok(None)
            }

            // [C6] /liststreams — list open tfopen file descriptors.
            "liststreams" => {
                if self.tf_files.is_empty() {
                    self.output.push("% No open streams.".to_owned());
                } else {
                    for (fd, f) in &self.tf_files {
                        let kind = match f {
                            TfFile::Reader(_)   => "r",
                            TfFile::Writer(_)   => "w",
                            TfFile::Appender(_) => "a",
                        };
                        self.output.push(format!("% stream {fd}: mode={kind}"));
                    }
                }
                Ok(None)
            }

            "option102" => {
                // /option102 [-w world] data
                // Sends IAC SB 102 <data> IAC SE to the specified (or active) world.
                let expanded = expand(args, self)?;
                let mut rest = expanded.trim();
                let mut world: Option<String> = None;
                if let Some(r) = rest.strip_prefix("-w") {
                    let r = r.trim_start();
                    let (w, tail) = r.split_once(char::is_whitespace).unwrap_or((r, ""));
                    world = Some(w.to_owned());
                    rest = tail.trim_start();
                }
                self.actions.push(ScriptAction::Option102 {
                    data: rest.as_bytes().to_vec(),
                    world,
                });
                Ok(None)
            }

            "dokey" => {
                // /dokey <op>  — apply a built-in key operation to the input editor.
                let expanded = expand(args, self)?;
                let op_name = expanded.trim();
                if let Some(op) = crate::keybind::DoKeyOp::from_name(op_name) {
                    self.actions.push(ScriptAction::DoKey(op));
                } else {
                    self.output.push(format!("% /dokey: unknown operation {:?}", op_name));
                }
                Ok(None)
            }

            "status" => {
                // /status [format]  — set the status bar format string.
                // Silently no-op when called with sub-command flags from
                // tfstatus.tf macros (status_add, status_rm, etc.) — those
                // are dispatched as macros, not as the /status built-in.
                let expanded = expand(args, self)?;
                let s = expanded.trim();
                if !s.is_empty() && !s.starts_with('-') {
                    self.actions.push(ScriptAction::SetStatus(s.to_owned()));
                }
                Ok(None)
            }

            // ── Help ──────────────────────────────────────────────────────────
            "help" => {
                self.output.push("TF commands: /connect /disconnect /world /addworld \
                    /def /undef /load /repeat /quote /log /nolog /list /listworlds \
                    /echo /eval /quit".to_owned());
                Ok(None)
            }

            // ── Expression / condition evaluation ─────────────────────────────
            // /test expr — evaluate expr and return its boolean value.
            // Used by /@test in /if conditions.
            "test" | "result" => {
                let expanded = expand(args, self)?;
                let val = eval_str(&expanded, self)?;
                Ok(Some(ControlFlow::Return(val)))
            }

            // /then body — execute body as a TF command.
            // Used in old-style TF "if cond%; /then body" patterns.
            "then" => {
                let expanded = expand(args, self)?;
                let body = expanded.trim();
                if body.is_empty() {
                    return Ok(None);
                }
                let stmts = parse_script(body)
                    .map_err(|e| format!("/then: parse error: {e}"))?;
                self.exec_block(&stmts)
            }

            // ── Lua scripting ──────────────────────────────────────────────────
            #[cfg(feature = "lua")]
            "loadlua" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::LuaLoad(expanded.trim().to_owned()));
                Ok(None)
            }

            #[cfg(feature = "lua")]
            "calllua" => {
                let expanded = expand(args, self)?;
                let mut parts = expanded.splitn(2, char::is_whitespace);
                let func = parts.next().unwrap_or("").to_owned();
                let rest = parts.next().unwrap_or("").trim().to_owned();
                let args: Vec<String> = if rest.is_empty() {
                    vec![]
                } else {
                    rest.split_whitespace().map(str::to_owned).collect()
                };
                self.actions.push(ScriptAction::LuaCall { func, args });
                Ok(None)
            }

            #[cfg(feature = "lua")]
            "purgelua" => {
                self.actions.push(ScriptAction::LuaPurge);
                Ok(None)
            }

            // ── Python scripting ───────────────────────────────────────────────
            #[cfg(feature = "python")]
            "python" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::PythonExec(expanded));
                Ok(None)
            }

            #[cfg(feature = "python")]
            "callpython" => {
                let expanded = expand(args, self)?;
                let mut parts = expanded.splitn(2, char::is_whitespace);
                let func = parts.next().unwrap_or("").to_owned();
                let arg = parts.next().unwrap_or("").trim().to_owned();
                self.actions.push(ScriptAction::PythonCall { func, arg });
                Ok(None)
            }

            #[cfg(feature = "python")]
            "loadpython" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::PythonLoad(expanded.trim().to_owned()));
                Ok(None)
            }

            #[cfg(feature = "python")]
            "killpython" => {
                self.actions.push(ScriptAction::PythonKill);
                Ok(None)
            }

            // ── Unknown command ────────────────────────────────────────────────
            _ => {
                self.actions.push(ScriptAction::LocalLine(format!(
                    "% Unknown command: /{name}"
                )));
                Ok(None)
            }
        }
    }

    /// Invoke a macro body (already parsed) with positional parameters.
    fn invoke_macro(
        &mut self,
        stmts: Vec<Stmt>,
        name: &str,
        params: Vec<String>,
    ) -> Result<Option<ControlFlow>, String> {
        self.frames.push(Frame {
            locals: HashMap::new(),
            params,
            cmd_name: name.to_owned(),
        });
        let result = self.exec_block(&stmts);
        self.frames.pop();
        result
    }
}

// ── Status field spec parser ───────────────────────────────────────────────────

/// Parse a `status_fields` spec string and return the nth colon-separated
/// attribute (0=name, 1=width, 2=label/flags) for the field whose name
/// matches `field`.  Field tokens are whitespace-separated; format is
/// `[name][:width[:label]]`.
fn status_field_attr(spec: &str, field: &str, attr: usize) -> Option<String> {
    for token in spec.split_whitespace() {
        let parts: Vec<&str> = token.splitn(3, ':').collect();
        if parts.first().copied().unwrap_or("") == field {
            return parts.get(attr).map(|s| s.to_string());
        }
    }
    None
}

// ── EvalContext impl ──────────────────────────────────────────────────────────

impl EvalContext for Interpreter {
    fn get_var(&self, name: &str) -> Option<Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.locals.get(name) {
                return Some(v.clone());
            }
        }
        self.globals.get(name).cloned()
    }

    fn set_local(&mut self, name: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.insert(name.to_owned(), value);
        } else {
            self.globals.insert(name.to_owned(), value);
        }
    }

    fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(name.to_owned(), value);
    }

    fn positional_params(&self) -> &[String] {
        self.frames.last().map(|f| f.params.as_slice()).unwrap_or(&[])
    }

    fn current_cmd_name(&self) -> &str {
        self.frames.last().map(|f| f.cmd_name.as_str()).unwrap_or("")
    }

    fn call_fn(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        // Functions that need interpreter state (params, locals, output).
        match name {
            "getopts" => return self.builtin_getopts(&args),
            "echo"    => return self.builtin_echo_fn(&args),
            "prompt"  => return self.builtin_prompt_fn(&args),
            "substitute" => return self.builtin_substitute_fn(&args),
            "isvar" => {
                let vname = args.first().map(|v| v.to_string()).unwrap_or_default();
                let exists = self.get_var(&vname).is_some();
                return Ok(Value::Int(if exists { 1 } else { 0 }));
            }
            "ismacro" => {
                let mname = args.first().map(|v| v.to_string()).unwrap_or_default();
                let exists = self.macros.contains_key(&mname) || self.macro_names.contains(&mname);
                return Ok(Value::Int(if exists { 1 } else { 0 }));
            }
            "worldname" => {
                return Ok(self.globals.get("worldname").cloned().unwrap_or_default());
            }
            "nworlds" => {
                return Ok(self.globals.get("nworlds").cloned().unwrap_or(Value::Int(0)));
            }
            "moresize" => {
                // moresize([mode]) — scrollback line count.
                // mode "ln" = new lines since limit marker (not tracked; return 0).
                // All other modes (including no arg) return the total scrollback count.
                let mode = args.first().map(|v| v.to_string()).unwrap_or_default();
                return Ok(if mode == "ln" {
                    Value::Int(0)
                } else {
                    self.globals.get("moresize").cloned().unwrap_or(Value::Int(0))
                });
            }
            "limit" => {
                // limit() — returns 1 if scroll-limit is active.  Not implemented; return 0.
                return Ok(Value::Int(0));
            }
            "kbpoint" => {
                return Ok(self.globals.get("kbpoint").cloned().unwrap_or(Value::Int(0)));
            }
            "kbhead" => {
                return Ok(self.globals.get("kbhead").cloned().unwrap_or_default());
            }
            "kbtail" => {
                return Ok(self.globals.get("kbtail").cloned().unwrap_or_default());
            }
            "idle" => {
                return Ok(self.globals.get("_idle").cloned().unwrap_or(Value::Float(0.0)));
            }
            "sidle" => {
                return Ok(self.globals.get("_sidle").cloned().unwrap_or(Value::Float(0.0)));
            }
            "fg_world" => {
                return Ok(self.globals.get("fg_world").cloned().unwrap_or_default());
            }
            "is_open" | "is_connected" => {
                let world = args.first().map(|v| v.to_string()).unwrap_or_default();
                let open = self.globals.get("_open_worlds")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let found = open.split_whitespace().any(|w| w == world);
                return Ok(Value::Int(if found { 1 } else { 0 }));
            }
            "nactive" => {
                return Ok(self.globals.get("nactive").cloned().unwrap_or(Value::Int(0)));
            }
            "columns" => {
                return Ok(self.globals.get("columns").cloned().unwrap_or(Value::Int(80)));
            }
            "winlines" | "lines" => {
                return Ok(self.globals.get("winlines").cloned().unwrap_or(Value::Int(24)));
            }
            // Status field introspection — parse the %status_fields variable.
            "status_fields" => {
                return Ok(self.globals.get("status_fields").cloned().unwrap_or_default());
            }
            "status_width" => {
                let name = args.first().map(|v| v.to_string()).unwrap_or_default();
                let spec = self.globals.get("status_fields").map(|v| v.to_string()).unwrap_or_default();
                let width = status_field_attr(&spec, &name, 1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
                return Ok(Value::Int(width));
            }
            "status_label" => {
                let name = args.first().map(|v| v.to_string()).unwrap_or_default();
                let spec = self.globals.get("status_fields").map(|v| v.to_string()).unwrap_or_default();
                let label = status_field_attr(&spec, &name, 2).unwrap_or_default();
                return Ok(Value::Str(label));
            }
            // ── File I/O ──────────────────────────────────────────────────────
            "tfopen" => {
                use std::fs::OpenOptions;
                let path = args.first().map(|v| v.to_string()).unwrap_or_default();
                let mode = args.get(1).map(|v| v.to_string()).unwrap_or_else(|| "r".to_owned());
                let result = match mode.as_str() {
                    "r" => std::fs::File::open(&path).map(|f| TfFile::Reader(std::io::BufReader::new(f))),
                    "w" => OpenOptions::new().write(true).create(true).truncate(true).open(&path).map(TfFile::Writer),
                    "a" => OpenOptions::new().create(true).append(true).open(&path).map(TfFile::Appender),
                    _ => {
                        return Ok(Value::Int(-1));
                    }
                };
                match result {
                    Ok(tf_file) => {
                        let fd = self.next_tf_fd;
                        self.next_tf_fd += 1;
                        self.tf_files.insert(fd, tf_file);
                        return Ok(Value::Int(fd));
                    }
                    Err(_) => return Ok(Value::Int(-1)),
                }
            }
            "tfclose" => {
                let fd = args.first().map(|v| v.to_string()).unwrap_or_default()
                    .parse::<i64>().unwrap_or(-1);
                let removed = self.tf_files.remove(&fd).is_some();
                return Ok(Value::Int(if removed { 1 } else { 0 }));
            }
            "tfread" => {
                use std::io::BufRead;
                let fd = args.first().map(|v| v.to_string()).unwrap_or_default()
                    .parse::<i64>().unwrap_or(-1);
                match self.tf_files.get_mut(&fd) {
                    Some(TfFile::Reader(r)) => {
                        let mut line = String::new();
                        match r.read_line(&mut line) {
                            Ok(0) => return Ok(Value::Int(-1)), // EOF
                            Ok(_) => {
                                if line.ends_with('\n') { line.pop(); }
                                if line.ends_with('\r') { line.pop(); }
                                return Ok(Value::Str(line));
                            }
                            Err(_) => return Ok(Value::Int(-1)),
                        }
                    }
                    _ => return Ok(Value::Int(-1)),
                }
            }
            "tfwrite" | "fwrite" => {
                use std::io::Write;
                let fd = args.first().map(|v| v.to_string()).unwrap_or_default()
                    .parse::<i64>().unwrap_or(-1);
                let text = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                let ok = match self.tf_files.get_mut(&fd) {
                    Some(TfFile::Writer(f) | TfFile::Appender(f)) => {
                        writeln!(f, "{text}").is_ok()
                    }
                    _ => false,
                };
                return Ok(Value::Int(if ok { 1 } else { 0 }));
            }
            "tfflush" => {
                use std::io::Write;
                let fd = args.first().map(|v| v.to_string()).unwrap_or_default()
                    .parse::<i64>().unwrap_or(-1);
                let ok = match self.tf_files.get_mut(&fd) {
                    Some(TfFile::Writer(f) | TfFile::Appender(f)) => f.flush().is_ok(),
                    _ => false,
                };
                return Ok(Value::Int(if ok { 1 } else { 0 }));
            }
            "tfreadable" => {
                let fd = args.first().map(|v| v.to_string()).unwrap_or_default()
                    .parse::<i64>().unwrap_or(-1);
                // A reader is "readable" if it still has data (buffer non-empty or file not at EOF).
                let readable = match self.tf_files.get_mut(&fd) {
                    Some(TfFile::Reader(r)) => {
                        use std::io::BufRead;
                        r.fill_buf().map(|b| !b.is_empty()).unwrap_or(false)
                    }
                    _ => false,
                };
                return Ok(Value::Int(if readable { 1 } else { 0 }));
            }

            // ── World introspection ───────────────────────────────────────────
            "world_info" => {
                // world_info(world, field) — query a world's definition.
                // Fields: "host", "port", "type", "character"/"char", "mfile".
                let world = args.first().map(|v| v.to_string()).unwrap_or_default();
                let field = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                let val = self.worlds_snapshot.get(&world).and_then(|info| {
                    match field.as_str() {
                        "host"                  => info[0].clone(),
                        "port"                  => info[1].clone(),
                        "type"                  => info[2].clone(),
                        "character" | "char"    => info[3].clone(),
                        "mfile"                 => info[4].clone(),
                        _                       => None,
                    }
                });
                return Ok(val.map(Value::Str).unwrap_or_default());
            }

            // ── Keyboard introspection / manipulation ─────────────────────────
            "kblen" => {
                // kblen() — length of the current input buffer in chars.
                let head_len = self.globals.get("kbhead").map(|v| v.to_string().chars().count()).unwrap_or(0);
                let tail_len = self.globals.get("kbtail").map(|v| v.to_string().chars().count()).unwrap_or(0);
                return Ok(Value::Int((head_len + tail_len) as i64));
            }
            "kbdel" => {
                // kbdel(n) — delete n chars forward from cursor; negative = backward.
                let n = args.first().map(|v| v.as_int()).unwrap_or(1);
                if n > 0 {
                    self.actions.push(ScriptAction::KbDel(n as usize));
                }
                return Ok(Value::Int(1));
            }
            "kbgoto" => {
                // kbgoto(pos) — move cursor to char position pos.
                let pos = args.first().map(|v| v.as_int()).unwrap_or(0).max(0) as usize;
                self.actions.push(ScriptAction::KbGoto(pos));
                return Ok(Value::Int(1));
            }
            "kbmatch" => {
                // kbmatch([pat]) — find first occurrence of pattern in input buffer.
                // Returns the 0-based char position, or -1 if not found.
                // Without an argument, returns the start of the current word.
                let head = self.globals.get("kbhead").map(|v| v.to_string()).unwrap_or_default();
                let tail = self.globals.get("kbtail").map(|v| v.to_string()).unwrap_or_default();
                let buf = format!("{head}{tail}");
                let pos = if args.is_empty() {
                    // No pattern: find start of current word (last whitespace before cursor).
                    let cursor = head.chars().count();
                    let head_chars: Vec<char> = head.chars().collect();
                    let word_start = head_chars.iter().rposition(|c| c.is_whitespace())
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    if word_start < cursor { word_start as i64 } else { -1 }
                } else {
                    let pat = args[0].to_string();
                    buf.char_indices()
                        .enumerate()
                        .find(|(_, (byte_pos, _))| buf[*byte_pos..].starts_with(&pat))
                        .map(|(char_pos, _)| char_pos as i64)
                        .unwrap_or(-1)
                };
                return Ok(Value::Int(pos));
            }

            // kbwordleft([n]) / kbwordright([n]) — cursor position n words left/right.
            // These return the new cursor position; they do NOT move the cursor.
            // Typically bound to M-b / M-f and wired to kbgoto() in user macros.
            "kbwordleft" => {
                let n = args.first().map(|v| v.as_int()).unwrap_or(1).max(0) as usize;
                let head = self.globals.get("kbhead").map(|v| v.to_string()).unwrap_or_default();
                let cursor = head.chars().count();
                let head_chars: Vec<char> = head.chars().collect();
                let mut pos = cursor;
                for _ in 0..n {
                    // Skip whitespace to the left, then skip non-whitespace.
                    while pos > 0 && head_chars[pos - 1].is_whitespace() { pos -= 1; }
                    while pos > 0 && !head_chars[pos - 1].is_whitespace() { pos -= 1; }
                }
                return Ok(Value::Int(pos as i64));
            }
            "kbwordright" => {
                let n = args.first().map(|v| v.as_int()).unwrap_or(1).max(0) as usize;
                let head = self.globals.get("kbhead").map(|v| v.to_string()).unwrap_or_default();
                let tail = self.globals.get("kbtail").map(|v| v.to_string()).unwrap_or_default();
                let cursor = head.chars().count();
                let buf = format!("{head}{tail}");
                let buf_chars: Vec<char> = buf.chars().collect();
                let total = buf_chars.len();
                let mut pos = cursor;
                for _ in 0..n {
                    // Skip leading whitespace, then skip non-whitespace (Emacs M-f
                    // style: cursor lands at end of next word, before trailing space).
                    while pos < total && buf_chars[pos].is_whitespace() { pos += 1; }
                    while pos < total && !buf_chars[pos].is_whitespace() { pos += 1; }
                }
                return Ok(Value::Int(pos as i64));
            }

            // ── Pager control ─────────────────────────────────────────────────
            "morepaused" => {
                return Ok(self.globals.get("_morepaused").cloned().unwrap_or(Value::Int(0)));
            }
            "morescroll" => {
                let n = args.first().map(|v| v.as_int()).unwrap_or(1);
                self.actions.push(ScriptAction::MoreScroll(n));
                return Ok(Value::Int(1));
            }

            // ── Session counters (synced from event loop) ──────────────────────
            "nlog" => {
                return Ok(self.globals.get("nlog").cloned().unwrap_or(Value::Int(0)));
            }
            "nmail" => {
                // Mail checking not implemented; return safe default.
                return Ok(Value::Int(0));
            }
            "nread" => {
                // Unread-mail count not implemented; return safe default.
                return Ok(Value::Int(0));
            }

            // ── Server injection ───────────────────────────────────────────────
            // send(text[, world[, flags]])
            // Flags: "u" = no trailing newline (unflushed/raw); "h" = fire SEND hook.
            // This is the primary way scripts send text to a MUD (stdlib.tf:113).
            "send" => {
                let text = args.first().map(|v| v.to_string()).unwrap_or_default();
                let world = if args.len() >= 2 && !args[1].to_string().is_empty() {
                    Some(args[1].to_string())
                } else {
                    None
                };
                let flags = args.get(2).map(|v| v.to_string()).unwrap_or_default();
                let no_newline = flags.contains('u');
                self.actions.push(ScriptAction::SendToWorld { text, world, no_newline });
                return Ok(Value::Int(1));
            }

            "fake_recv" => {
                // fake_recv([world,] line) — inject a line as if from the server.
                // If two args are given, first is the world name.
                let (world, line) = if args.len() >= 2 {
                    (Some(args[0].to_string()), args[1].to_string())
                } else {
                    (None, args.first().map(|v| v.to_string()).unwrap_or_default())
                };
                self.actions.push(ScriptAction::FakeRecv { world, line });
                return Ok(Value::Int(1));
            }

            _ => {}
        }

        if let Some(result) = call_builtin(name, args.clone()) {
            return result;
        }
        if self.macros.contains_key(name) {
            let stmts = self.macros.get_mut(name).unwrap().stmts()?.to_vec();
            let params: Vec<String> = args.iter().map(|v| v.to_string()).collect();
            self.invoke_macro(stmts, name, params)?;
            return Ok(Value::default());
        }
        // Unknown functions return empty string, matching TF's C behaviour.
        Ok(Value::default())
    }

    fn eval_expr_str(&mut self, s: &str) -> Result<Value, String> {
        eval_str(s, self)
    }
}

// ── Interpreter-aware builtin functions ────────────────────────────────────

impl Interpreter {

    /// `getopts(format[, default_args])` — parse option flags from the
    /// current macro's positional parameters.
    ///
    /// `format` is a string of flag characters; `:` after a char means that
    /// flag takes a value argument.  For each flag `-X` found in the params,
    /// sets local variable `opt_X` to `1` (boolean) or to the flag value
    /// (when `:` is present).  Remaining non-option params replace the frame's
    /// positional params.
    ///
    /// Returns `Value::Int(1)` on success, `Value::Int(0)` on error (unknown
    /// flag, missing value, etc.).
    fn builtin_getopts(&mut self, args: &[Value]) -> Result<Value, String> {
        let fmt = match args.first() {
            Some(v) => v.as_str(),
            None => return Ok(Value::Int(0)),
        };
        let default_args = args.get(1).map(|v| v.as_str());

        // Build flag table: flag_char → takes_value
        let mut flags: std::collections::HashMap<char, bool> = std::collections::HashMap::new();
        let fmt_chars: Vec<char> = fmt.chars().collect();
        let mut fi = 0;
        while fi < fmt_chars.len() {
            let c = fmt_chars[fi];
            if c == ':' { fi += 1; continue; }
            let takes_val = fmt_chars.get(fi + 1) == Some(&':');
            flags.insert(c, takes_val);
            fi += 1;
        }

        // Get current positional params (use default_args if no params).
        let params: Vec<String> = {
            let p = self.positional_params();
            if p.is_empty() {
                if let Some(def) = default_args {
                    if def.is_empty() {
                        vec![]
                    } else {
                        def.split_whitespace().map(str::to_owned).collect()
                    }
                } else {
                    vec![]
                }
            } else {
                p.to_vec()
            }
        };

        let mut i = 0;
        let mut remaining: Vec<String> = Vec::new();
        let mut ok = true;

        while i < params.len() {
            let arg = &params[i];
            if arg == "--" {
                // End of options; everything after goes to remaining.
                i += 1;
                remaining.extend(params[i..].iter().cloned());
                break;
            }
            if !arg.starts_with('-') || arg == "-" {
                // Not an option; collect as remaining.
                remaining.extend(params[i..].iter().cloned());
                break;
            }
            // Parse flags from this token (e.g., "-pAw" or "-w world").
            let mut chars = arg[1..].chars().peekable();
            while let Some(c) = chars.next() {
                match flags.get(&c) {
                    None => {
                        // Unknown flag.
                        ok = false;
                        break;
                    }
                    Some(true) => {
                        // Flag takes a value.
                        let val: String = chars.collect(); // rest of this token
                        let val = if val.is_empty() {
                            // Value is the next positional param.
                            i += 1;
                            params.get(i).cloned().unwrap_or_default()
                        } else {
                            val
                        };
                        self.set_local(&format!("opt_{c}"), Value::Str(val));
                        break; // consumed rest of token as value
                    }
                    Some(false) => {
                        // Boolean flag.
                        self.set_local(&format!("opt_{c}"), Value::Int(1));
                    }
                }
            }
            if !ok { break; }
            i += 1;
        }

        // Replace the frame's positional params with the remaining args.
        if let Some(frame) = self.frames.last_mut() {
            frame.params = remaining;
        }

        Ok(Value::Int(if ok { 1 } else { 0 }))
    }

    /// `echo(text[, attr[, pageable[, dest]]])` — output a line.
    /// This is the function form called from within macro bodies.
    fn builtin_echo_fn(&mut self, args: &[Value]) -> Result<Value, String> {
        let text = args.first().map(|v| v.as_str()).unwrap_or_default();
        // attr, pageable, dest are accepted but ignored in this implementation.
        self.output.push(text);
        Ok(Value::Int(1))
    }

    /// `prompt(text[, attr[, pageable]])` — set the input-line prompt text.
    fn builtin_prompt_fn(&mut self, args: &[Value]) -> Result<Value, String> {
        let text = args.first().map(|v| v.as_str()).unwrap_or_default();
        self.output.push(format!("[prompt] {text}"));
        Ok(Value::Int(1))
    }

    /// `substitute(text[, attr[, pageable]])` — substitute the current line.
    fn builtin_substitute_fn(&mut self, args: &[Value]) -> Result<Value, String> {
        let text = args.first().map(|v| v.as_str()).unwrap_or_default();
        self.output.push(text);
        Ok(Value::Int(1))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a string as an integer or float if possible; otherwise keep as Str.
fn try_parse_number(s: &str) -> Value {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        Value::Int(n)
    } else if let Ok(x) = t.parse::<f64>() {
        Value::Float(x)
    } else {
        Value::Str(s.to_owned())
    }
}

/// Parse `/def [-flags…] [name] [= body]` into a [`Macro`].
///
/// Handles the most common flags: `-t`, `-h`, `-p`, `-P`, `-n`, `-w`, `-b`,
/// `-B`, `-T`, `-E`, `-f`, `-q`, `-i`.  Unknown flags are silently skipped.
fn parse_def(raw: &str) -> Macro {
    let mut mac = Macro::new();
    let mut rest = raw.trim();
    let mut mode = MatchMode::Glob; // TF default trigger mode

    while rest.starts_with('-') {
        // Consume the flag token (may have the value embedded, e.g. `-t pattern`
        // can be written as `-tpattern`).
        let tok_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let tok = &rest[1..tok_end]; // everything after the leading `-`
        rest = rest[tok_end..].trim_start();

        if tok.is_empty() {
            continue;
        }
        let flag = &tok[..1];
        let inline = if tok.len() > 1 { &tok[1..] } else { "" };

        // Helper: get flag value — inline if present, else consume next token.
        let get_val = |inline: &str, rest: &mut &str| -> String {
            if !inline.is_empty() {
                inline.to_owned()
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let v = rest[..end].to_owned();
                *rest = rest[end..].trim_start();
                v
            }
        };

        match flag {
            "t" => {
                let pat = get_val(inline, &mut rest);
                if !pat.is_empty() {
                    mac.trig = Pattern::new(&pat, mode).ok();
                }
            }
            "h" => {
                let spec = get_val(inline, &mut rest);
                // Spec format: HOOKNAME[|HOOKNAME2][/hargs-pattern]
                let (hook_part, hargs_part) = match spec.find('/') {
                    Some(idx) => (&spec[..idx], Some(&spec[idx + 1..])),
                    None => (spec.as_str(), None),
                };
                let mut hs = HookSet::NONE;
                for name in hook_part.split('|') {
                    if let Ok(h) = name.trim().parse::<Hook>() {
                        hs.insert(h);
                    }
                }
                mac.hooks = hs;
                if let Some(pat) = hargs_part {
                    if !pat.is_empty() {
                        mac.hargs = Pattern::new(pat, MatchMode::Glob).ok();
                    }
                }
            }
            "p" => {
                let v = get_val(inline, &mut rest);
                mac.priority = v.parse().unwrap_or(1);
            }
            "P" => {
                let v = get_val(inline, &mut rest);
                mac.probability = v.parse::<u8>().unwrap_or(100).min(100);
            }
            "n" => {
                let v = get_val(inline, &mut rest);
                mac.shots = v.parse().unwrap_or(0);
            }
            "w" => {
                let v = get_val(inline, &mut rest);
                mac.world = Some(v);
            }
            "b" => {
                let v = get_val(inline, &mut rest);
                mac.bind = Some(v);
            }
            "B" => {
                let v = get_val(inline, &mut rest);
                mac.keyname = Some(v);
            }
            "T" => {
                let v = get_val(inline, &mut rest);
                mac.wtype = Pattern::new(&v, MatchMode::Glob).ok();
            }
            "E" => {
                let v = get_val(inline, &mut rest);
                mac.expr = Some(v);
            }
            "s" => {
                // Pattern mode: exact=simple, glob, regexp, substr
                let v = get_val(inline, &mut rest);
                mode = match v.as_str() {
                    "regexp" | "re" => MatchMode::Regexp,
                    "simple" | "exact" => MatchMode::Simple,
                    "substr" | "sub" => MatchMode::Substr,
                    _ => MatchMode::Glob,
                };
            }
            "f" => mac.fallthru = true,
            "q" => mac.quiet = true,
            "i" => mac.invisible = true,
            // -g (gag), -a (attr), -c (color) — skip value
            "g" | "a" | "c" | "m" => {
                get_val(inline, &mut rest);
            }
            _ => {
                // Unknown single-char flag with possible value — skip value
                // if the flag letter is lowercase and typically takes an arg.
                if !inline.is_empty() {
                    // value was inline, already consumed
                } else if tok.len() == 1 {
                    // might be a boolean flag, nothing to skip
                }
            }
        }
    }

    // rest = "[name] [= body]"  or  "" (bare `/def` — listing, ignored)
    if let Some(eq) = rest.find('=') {
        let name = rest[..eq].trim();
        let body = rest[eq + 1..].trim();
        if !name.is_empty() {
            mac.name = Some(name.to_owned());
        }
        mac.body = Some(body.to_owned());
    } else if !rest.is_empty() {
        mac.name = Some(rest.to_owned());
    }

    mac
}

/// Extract the `-q` (quiet) flag and remaining path from `/load` args.
fn parse_load_flags(args: &str) -> (bool, &str) {
    let mut rest = args.trim();
    let mut quiet = false;
    loop {
        if let Some(r) = rest.strip_prefix("-q") {
            quiet = true;
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix("-L ").or_else(|| rest.strip_prefix("-L\t")) {
            // -L <dir> — skip the next token (we ignore the dir override here;
            // the caller's variable expansion handles TFLIBDIR).
            let end = r.find(char::is_whitespace).unwrap_or(r.len());
            rest = r[end..].trim_start();
        } else {
            break;
        }
    }
    (quiet, rest)
}

/// Parse `/repeat [-w world] interval [count] body` into `(interval_ms, count, body, world)`.
///
/// - `interval` is in seconds (may be a float); converted to milliseconds.
/// - `count` is optional; if the second token is also a positive integer it is
///   the count, otherwise count defaults to `None` (run forever).
/// - Everything after the interval (and optional count) is the body.
fn parse_repeat_args(s: &str) -> (u64, Option<u32>, String, Option<String>) {
    let mut rest = s;
    let mut world: Option<String> = None;

    // Optional `-w <world>` flag.
    if let Some(r) = rest.strip_prefix("-w").map(str::trim_start) {
        let end = r.find(char::is_whitespace).unwrap_or(r.len());
        world = Some(r[..end].to_owned());
        rest = r[end..].trim_start();
    }

    let mut parts = rest.splitn(3, char::is_whitespace);
    let interval_str = parts.next().unwrap_or("0");
    let interval_secs: f64 = interval_str.trim().parse().unwrap_or(0.0);
    let interval_ms = (interval_secs * 1000.0).max(1.0) as u64;

    let second = parts.next().unwrap_or("").trim();
    let (count, body) = if let Ok(n) = second.parse::<u32>() {
        let b = parts.next().unwrap_or("").trim().to_owned();
        (Some(n), b)
    } else {
        // second token is the beginning of the body
        let tail = parts.next().unwrap_or("").trim();
        let b = if tail.is_empty() {
            second.to_owned()
        } else {
            format!("{second} {tail}")
        };
        (None, b)
    };

    (interval_ms, count, body, world)
}

/// Parse an optional leading `/interval` prefix from `/quote` args.
///
/// TF syntax: `/quote [/seconds] 'file` or `/quote [/seconds] !cmd`
///
/// Returns `(interval_ms, remaining_str)`.  The default interval is 250 ms.
fn parse_quote_interval(s: &str) -> (u64, &str) {
    const DEFAULT_MS: u64 = 250;
    if let Some(rest) = s.strip_prefix('/') {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let tok = &rest[..end];
        if let Ok(n) = tok.parse::<f64>() {
            let ms = (n * 1000.0).max(1.0) as u64;
            return (ms, rest[end..].trim_start());
        }
        // '/' was not a numeric interval prefix; leave s untouched.
    }
    (DEFAULT_MS, s)
}

/// Tokenise an `/addworld` argument string (respects double-quoted tokens).
fn tokenise_addworld(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_q = !in_q,
            '\\' if in_q => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

// ── trigpc PRNG ───────────────────────────────────────────────────────────────

/// Return a pseudo-random number in `0..100` for `/trigpc` probability checks.
///
/// Uses a xorshift64 PRNG seeded from system time on first call.  Not
/// cryptographically secure, but suitable for MUD trigger probability.
fn trigpc_roll() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xdeadbeef);
        if s == 0 { s = 0xdeadbeef; }
    }
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    STATE.store(s, Ordering::Relaxed);
    s % 100
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Interpreter {
        let mut interp = Interpreter::new();
        interp.exec_script(src).expect("exec failed");
        interp
    }

    fn output(src: &str) -> Vec<String> {
        run(src).output
    }

    #[test]
    fn echo_basic() {
        assert_eq!(output("/echo Hello"), vec!["Hello"]);
    }

    #[test]
    fn echo_var_expand() {
        let mut interp = Interpreter::new();
        interp.set_global_var("name", Value::Str("Alice".into()));
        interp.exec_script("/echo Hello, %{name}!").unwrap();
        assert_eq!(interp.output, vec!["Hello, Alice!"]);
    }

    #[test]
    fn set_and_get_global() {
        let mut interp = Interpreter::new();
        interp.exec_script("/set x=10").unwrap();
        assert_eq!(interp.get_global_var("x"), Some(&Value::Int(10)));
    }

    #[test]
    fn if_true() {
        let src = "/set x=5\n/if (x > 3)\n/echo yes\n/endif";
        assert_eq!(output(src), vec!["yes"]);
    }

    #[test]
    fn if_false() {
        let src = "/set x=1\n/if (x > 3)\n/echo yes\n/else\n/echo no\n/endif";
        assert_eq!(output(src), vec!["no"]);
    }

    #[test]
    fn while_loop() {
        let src = "/set i=0\n/while (i < 3)\n/echo loop\n/set i=$[i+1]\n/done";
        let out = output(src);
        assert_eq!(out, vec!["loop", "loop", "loop"]);
    }

    #[test]
    fn while_break() {
        let src = "/set i=0\n/while (1)\n/set i=%{i}\n/break\n/done";
        let out = output(src);
        assert!(out.is_empty());
    }

    #[test]
    fn return_from_script() {
        let mut interp = Interpreter::new();
        let v = interp.exec_script("/return 42").unwrap();
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn expr_in_echo() {
        let src = "/echo result=$[3 * 7]";
        assert_eq!(output(src), vec!["result=21"]);
    }

    #[test]
    fn macro_call() {
        let mut interp = Interpreter::new();
        interp.define_macro("greet", "/echo Hello, {1}!");
        interp.exec_script("/greet World").unwrap();
        assert_eq!(interp.output, vec!["Hello, World!"]);
    }

    #[test]
    fn builtin_strlen_via_expr() {
        let src = "/echo $[strlen(\"hello\")]";
        assert_eq!(output(src), vec!["5"]);
    }

    #[test]
    fn local_scope_isolation() {
        let mut interp = Interpreter::new();
        interp.set_global_var("x", Value::Int(99));
        interp.define_macro("setx", "/let x=42");
        interp.exec_script("/setx").unwrap();
        assert_eq!(interp.get_global_var("x"), Some(&Value::Int(99)));
    }

    #[test]
    fn for_range_loop() {
        let src = "/for i 1 3 /echo %i";
        assert_eq!(output(src), vec!["1", "2", "3"]);
    }

    #[test]
    fn for_range_sets_var() {
        let src = "/for n 10 12 /set last=%n\n/echo %last";
        let out = output(src);
        assert_eq!(out, vec!["12"]);
    }

    #[test]
    fn send_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/send go east").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::SendToWorld { text, world: None, no_newline: false } if text == "go east"
        ));
    }

    #[test]
    fn quit_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quit").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(actions[0], ScriptAction::Quit));
    }

    #[test]
    fn def_and_call() {
        let mut interp = Interpreter::new();
        interp.exec_script("/def -i greet = /echo Hi, {1}!").unwrap();
        interp.exec_script("/greet World").unwrap();
        assert_eq!(interp.output, vec!["Hi, World!"]);
    }

    #[test]
    fn load_via_file_loader() {
        let body = Arc::new(|_path: &str| Ok("/echo loaded".to_owned()));
        let mut interp = Interpreter::new();
        interp.file_loader = Some(body);
        interp.exec_script("/load somefile.tf").unwrap();
        assert_eq!(interp.output, vec!["% Loading commands from somefile.tf.", "loaded"]);
    }

    #[test]
    fn eval_command() {
        let out = output("/eval /echo from eval");
        assert_eq!(out, vec!["from eval"]);
    }

    #[test]
    fn connect_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/connect mymud").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::Connect { name } if name == "mymud"));
    }

    // ── Phase 13: /def flag parsing ───────────────────────────────────────────

    #[test]
    fn def_queues_def_macro_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/def foo = /echo bar").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::DefMacro(m) if m.name.as_deref() == Some("foo")));
    }

    #[test]
    fn def_trigger_flag_parsed() {
        let mac = parse_def("-t ^hello name = body");
        assert!(mac.trig.is_some(), "trigger pattern should be set");
        assert_eq!(mac.name.as_deref(), Some("name"));
        assert_eq!(mac.body.as_deref(), Some("body"));
    }

    #[test]
    fn def_priority_flag_parsed() {
        let mac = parse_def("-p 10 foo = bar");
        assert_eq!(mac.priority, 10);
    }

    #[test]
    fn def_hook_flag_parsed() {
        let mac = parse_def("-h CONNECT foo = bar");
        assert!(!mac.hooks.is_empty(), "hooks should be set");
    }

    #[test]
    fn def_fallthru_quiet_invisible() {
        let mac = parse_def("-f -q -i foo = bar");
        assert!(mac.fallthru);
        assert!(mac.quiet);
        assert!(mac.invisible);
    }

    #[test]
    fn def_probability_flag() {
        let mac = parse_def("-P 75 foo = body");
        assert_eq!(mac.probability, 75);
    }

    // ── Phase 14: process scheduling, undef, unbind, logging ─────────────────

    #[test]
    fn repeat_queues_add_repeat() {
        let mut interp = Interpreter::new();
        interp.exec_script("/repeat 2 /echo hi").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::AddRepeat { interval_ms, count, body, world: None }
            if *interval_ms == 2000 && count.is_none() && body == "/echo hi"
        ));
    }

    #[test]
    fn repeat_with_count_queues_correctly() {
        let mut interp = Interpreter::new();
        interp.exec_script("/repeat 1 5 hello").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::AddRepeat { interval_ms, count, body, world: None }
            if *interval_ms == 1000 && *count == Some(5) && body == "hello"
        ));
    }

    #[test]
    fn quote_file_queues_add_quote_file() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quote 'myfile.txt").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::AddQuoteFile { path, .. } if path == "myfile.txt"
        ));
    }

    #[test]
    fn quote_shell_queues_add_quote_shell() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quote !ls").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::AddQuoteShell { command, .. } if command == "ls"
        ));
    }

    #[test]
    fn quote_with_interval_queues_with_ms() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quote /0.5 'file.txt").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::AddQuoteFile { interval_ms, .. } if *interval_ms == 500
        ));
    }

    #[test]
    fn undef_queues_undef_macro() {
        let mut interp = Interpreter::new();
        interp.exec_script("/def foo = /echo hi").unwrap();
        let _ = interp.take_actions();
        interp.exec_script("/undef foo").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::UndefMacro(n) if n == "foo"));
    }

    #[test]
    fn unbind_queues_unbind_key() {
        let mut interp = Interpreter::new();
        interp.exec_script("/unbind ^A").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::UnbindKey(s) if s == "^A"));
    }

    #[test]
    fn log_queues_start_log() {
        let mut interp = Interpreter::new();
        interp.exec_script("/log /tmp/test.log").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::StartLog(p) if p == "/tmp/test.log"));
    }

    #[test]
    fn nolog_queues_stop_log() {
        let mut interp = Interpreter::new();
        interp.exec_script("/nolog").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::StopLog));
    }

    #[test]
    fn listworlds_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/listworlds").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::ListWorlds));
    }

    #[test]
    fn list_with_filter_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/list foo").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::ListMacros { filter: Some(f) } if f == "foo"
        ));
    }

    #[test]
    fn parse_repeat_args_fraction_seconds() {
        let (ms, count, body, world) = parse_repeat_args("0.5 walk");
        assert_eq!(ms, 500);
        assert!(count.is_none());
        assert_eq!(body, "walk");
        assert!(world.is_none());
    }

    #[test]
    fn parse_quote_interval_default() {
        let (ms, rest) = parse_quote_interval("'file.txt");
        assert_eq!(ms, 250);
        assert_eq!(rest, "'file.txt");
    }

    #[test]
    fn parse_quote_interval_explicit() {
        let (ms, rest) = parse_quote_interval("/1 'file.txt");
        assert_eq!(ms, 1000);
        assert_eq!(rest, "'file.txt");
    }

    #[test]
    fn export_sets_env_var() {
        let mut interp = Interpreter::new();
        interp.set_global_var("TESTEXPORT_XYZ", Value::Str("hello_world".into()));
        interp.exec_script("/export TESTEXPORT_XYZ").unwrap();
        assert_eq!(std::env::var("TESTEXPORT_XYZ").unwrap_or_default(), "hello_world");
    }

    #[test]
    fn export_undefined_var_outputs_error() {
        let mut interp = Interpreter::new();
        interp.exec_script("/export NO_SUCH_VAR_EVER_DEFINED").unwrap();
        assert!(!interp.output.is_empty());
        assert!(interp.output[0].contains("not defined"));
    }

    #[test]
    fn quote_sync_file_queues_sync_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quote -S 'myfile.txt").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::QuoteFileSync { path, .. } if path == "myfile.txt"
        ));
    }

    #[test]
    fn quote_sync_shell_queues_sync_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quote -S !echo hi").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::QuoteShellSync { command, .. } if command == "echo hi"
        ));
    }

    // ── /restrict tests ─────────────────────────────────────────────────────

    #[test]
    fn restrict_prints_level_when_no_args() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict").unwrap();
        assert!(interp.output[0].contains("none"));
    }

    #[test]
    fn restrict_raises_level() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict shell").unwrap();
        assert_eq!(interp.restriction, RestrictionLevel::Shell);
        interp.exec_script("/restrict file").unwrap();
        assert_eq!(interp.restriction, RestrictionLevel::File);
    }

    #[test]
    fn restrict_cannot_lower_level() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict file").unwrap();
        interp.output.clear();
        interp.exec_script("/restrict shell").unwrap();
        assert_eq!(interp.restriction, RestrictionLevel::File);
        assert!(interp.output[0].contains("can not be lowered"));
    }

    #[test]
    fn restrict_shell_blocks_sh_command() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict shell").unwrap();
        interp.output.clear();
        interp.exec_script("/sh echo hello").unwrap();
        assert!(interp.output[0].contains("restricted"));
        assert!(interp.actions.is_empty());
    }

    #[test]
    fn restrict_shell_blocks_quote_shell() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict shell").unwrap();
        interp.exec_script("/quote !echo hello").unwrap();
        assert!(interp.actions.is_empty());
    }

    #[test]
    fn restrict_file_blocks_load() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict file").unwrap();
        interp.output.clear();
        // No file_loader set, but restriction check happens before loader call.
        interp.exec_script("/load somefile.tf").unwrap();
        assert!(interp.output[0].contains("restricted"));
    }

    #[test]
    fn restrict_file_blocks_log() {
        let mut interp = Interpreter::new();
        interp.exec_script("/restrict file").unwrap();
        interp.output.clear();
        interp.exec_script("/log /tmp/test.log").unwrap();
        assert!(interp.output[0].contains("restricted"));
        assert!(interp.actions.is_empty());
    }

    // ── /suspend tests ──────────────────────────────────────────────────────

    #[test]
    fn suspend_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/suspend").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::Suspend));
    }

    // ── option102 tests ─────────────────────────────────────────────────────

    #[test]
    fn option102_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/option102 hello").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::Option102 { data, world: None } if data == b"hello"
        ));
    }

    #[test]
    fn option102_with_world_flag() {
        let mut interp = Interpreter::new();
        interp.exec_script("/option102 -w myworld hello").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::Option102 { data, world: Some(w) }
                if data == b"hello" && w == "myworld"
        ));
    }

    // ── send() function tests ────────────────────────────────────────────────

    #[test]
    fn send_fn_queues_action() {
        let mut interp = Interpreter::new();
        // /test evaluates an expression; send() should queue SendToWorld.
        interp.exec_script("/test send(\"go west\")").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::SendToWorld { text, world: None, no_newline: false }
                if text == "go west"
        ));
    }

    #[test]
    fn send_fn_with_world_arg() {
        let mut interp = Interpreter::new();
        interp.exec_script("/test send(\"hi\", \"myworld\")").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::SendToWorld { text, world: Some(w), no_newline: false }
                if text == "hi" && w == "myworld"
        ));
    }

    #[test]
    fn send_fn_unflushed_flag() {
        let mut interp = Interpreter::new();
        interp.exec_script("/test send(\"raw\", \"\", \"u\")").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::SendToWorld { text, world: None, no_newline: true }
                if text == "raw"
        ));
    }

    // ── kbwordleft / kbwordright tests ───────────────────────────────────────

    #[test]
    fn kbwordleft_basic() {
        let mut interp = Interpreter::new();
        // kbhead = "hello world", cursor is at end (position 11).
        interp.set_global_var("kbhead", Value::Str("hello world".to_owned()));
        interp.set_global_var("kbtail", Value::Str(String::new()));
        // One word left from "hello world|" should land at position 6 (start of "world").
        let result = interp.call_fn("kbwordleft", vec![Value::Int(1)]).unwrap();
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn kbwordright_basic() {
        let mut interp = Interpreter::new();
        // kbhead = "hello ", kbtail = "world here".
        interp.set_global_var("kbhead", Value::Str("hello ".to_owned()));
        interp.set_global_var("kbtail", Value::Str("world here".to_owned()));
        // One word right from position 6 should reach position 11 (after "world").
        let result = interp.call_fn("kbwordright", vec![Value::Int(1)]).unwrap();
        assert_eq!(result, Value::Int(11));
    }

    // ── /gag, /hilite, /relimit, /unlimit, /core, /edit tests ───────────────

    #[test]
    fn gag_no_args_sets_global() {
        let mut interp = Interpreter::new();
        interp.exec_script("/gag").unwrap();
        assert_eq!(interp.get_global_var("gag"), Some(&Value::Int(1)));
    }

    #[test]
    fn hilite_no_args_sets_global() {
        let mut interp = Interpreter::new();
        interp.exec_script("/hilite").unwrap();
        assert_eq!(interp.get_global_var("hilite"), Some(&Value::Int(1)));
    }

    #[test]
    fn relimit_sets_more() {
        let mut interp = Interpreter::new();
        interp.exec_script("/unlimit").unwrap();
        assert_eq!(interp.get_global_var("more"), Some(&Value::Int(0)));
        interp.exec_script("/relimit").unwrap();
        assert_eq!(interp.get_global_var("more"), Some(&Value::Int(1)));
    }

    #[test]
    fn core_outputs_message() {
        let mut interp = Interpreter::new();
        interp.exec_script("/core").unwrap();
        assert!(!interp.output.is_empty());
    }

    #[test]
    fn edit_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/edit").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::EditInput));
    }
}
