//! Main async event loop.
//!
//! Corresponds to `main_loop()` in `socket.c` and the dispatch in `main.c`,
//! `signals.c`, and `timers.c`.
//!
//! ## Architecture
//!
//! Each MUD connection runs in its own [`tokio::spawn`]ed task
//! (`connection_task`).  Events received from servers are forwarded through
//! an [`mpsc`] channel to the single [`EventLoop::run`] task, which also
//! drives keyboard input and handles OS signals.
//!
//! ```text
//!   ┌─────────────────────────┐
//!   │  EventLoop::run()       │
//!   │  tokio::select! over:   │
//!   │  • net_rx (all worlds)  │◄── connection_task (world A)
//!   │  • stdin                │◄── connection_task (world B)
//!   │  • SIGWINCH             │     ...
//!   │  • SIGTERM / SIGINT     │
//!   │  • timer (processes,    │
//!   │    mail, refresh)       │
//!   └─────────────────────────┘
//! ```

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use libc;

use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::attr::Attr;
use crate::hook::Hook;
use crate::keybind::{DoKeyOp, EditAction, InputProcessor, KeyBinding, Keymap};
use crate::macros::MacroStore;
use crate::net::{Connection, NetEvent};
use crate::process::ProcessScheduler;
use crate::screen::{LogicalLine, Screen};
use crate::script::interp::{FileLoader, Interpreter, ScriptAction};
use crate::terminal::{StatusLine, Terminal};
use crate::world::WorldStore;

// ── Timing constants ─────────────────────────────────────────────────────

const MAIL_CHECK_INTERVAL: Duration = Duration::from_secs(60);
const REFRESH_INTERVAL: Duration = Duration::from_millis(50);
/// How long to wait after receiving incomplete data before flushing it as a prompt.
/// Matches C TF's default `%prompt_sec` / `%prompt_usec` of 0.1 s.
const PROMPT_FLUSH_DELAY: Duration = Duration::from_millis(100);

// ── Keyboard byte → EditAction decoder ───────────────────────────────────

/// Accumulates raw bytes from stdin and translates them into [`EditAction`]s
/// by matching against the active [`Keymap`].
///
/// Multi-byte escape sequences (e.g. `ESC [ A` for Up Arrow) are handled by
/// buffering until a complete sequence is recognised.
struct KeyDecoder {
    buf: Vec<u8>,
    keymap: Keymap,
}

impl KeyDecoder {
    fn new() -> Self {
        Self { buf: Vec::new(), keymap: Keymap::new().with_defaults() }
    }

    /// Push one byte, returning an `EditAction` if a complete sequence was
    /// recognised, or `None` to signal that more bytes are expected.
    fn push(&mut self, b: u8) -> Option<EditAction> {
        self.buf.push(b);

        // Try exact match in keymap.
        if let Some(binding) = self.keymap.lookup(&self.buf) {
            let action = EditAction::Bound(binding.clone());
            self.buf.clear();
            return Some(action);
        }

        // Single printable ASCII byte → InsertChar.
        if self.buf.len() == 1 && !b.is_ascii_control() {
            self.buf.clear();
            return Some(EditAction::InsertChar(b as char));
        }

        // If the buffer is a prefix of some binding, wait for more bytes.
        if !self.keymap.has_prefix(&self.buf) {
            // Unknown sequence — discard and start fresh.
            self.buf.clear();
        }
        None
    }
}

// ── Per-connection task ───────────────────────────────────────────────────

/// Message sent *to* a connection task (raw bytes to write to the server).
type ToServer = Vec<u8>;

/// Message received *from* a connection task.
#[derive(Debug)]
pub struct NetMessage {
    pub world: String,
    pub event: NetEvent,
}

/// Spawned once per connection.  Owns the [`Connection`] and bridges it to
/// the [`EventLoop`] via channels.
async fn connection_task(
    mut conn: Connection,
    mut cmd_rx: mpsc::Receiver<ToServer>,
    event_tx: mpsc::Sender<NetMessage>,
    world: String,
) {
    // When the server sends data that doesn't end with a newline (e.g., a login
    // prompt like "Name: "), the bytes sit in the telnet line buffer forever.
    // We arm a short timer after each such receive; if no more data arrives
    // within PROMPT_FLUSH_DELAY, we emit the buffered bytes as a Prompt event —
    // matching C TF's %prompt_sec / %prompt_usec behaviour.
    let mut prompt_deadline: Option<tokio::time::Instant> = None;

    loop {
        // Build a future for the prompt timer that is Pending when not armed.
        let prompt_sleep = async {
            match prompt_deadline {
                Some(dl) => tokio::time::sleep_until(dl).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            result = conn.recv() => {
                match result {
                    Ok(events) => {
                        let had_complete_lines = events.iter().any(|e| matches!(e, NetEvent::Line(_)));
                        for ev in events {
                            let closed = matches!(ev, NetEvent::Closed);
                            if event_tx
                                .send(NetMessage { world: world.clone(), event: ev })
                                .await
                                .is_err()
                            {
                                return; // EventLoop shut down
                            }
                            if closed {
                                return;
                            }
                        }
                        // Arm or reset the prompt timer depending on whether
                        // there is still pending (non-newline-terminated) data.
                        if conn.has_pending() {
                            // (Re-)arm: data arrived but no complete line yet.
                            prompt_deadline =
                                Some(tokio::time::Instant::now() + PROMPT_FLUSH_DELAY);
                        } else if had_complete_lines {
                            // Complete lines consumed any partial buffer — disarm.
                            prompt_deadline = None;
                        }
                    }
                    Err(_) => {
                        let _ = event_tx
                            .send(NetMessage { world: world.clone(), event: NetEvent::Closed })
                            .await;
                        return;
                    }
                }
            }
            Some(bytes) = cmd_rx.recv() => {
                if conn.send_raw(&bytes).await.is_err() {
                    return;
                }
            }
            _ = prompt_sleep => {
                // Timer fired: flush buffered partial bytes as a Prompt.
                if let Some(ev) = conn.take_pending_as_prompt() {
                    let _ = event_tx
                        .send(NetMessage { world: world.clone(), event: ev })
                        .await;
                }
                prompt_deadline = None;
            }
            else => return,
        }
    }
}

// ── ConnectionHandle ──────────────────────────────────────────────────────

/// Lightweight handle for sending data to a live connection task.
pub struct ConnectionHandle {
    pub world_name: String,
    sender: mpsc::Sender<ToServer>,
}

impl ConnectionHandle {
    /// Write raw bytes to the server.
    pub async fn send_raw(&self, bytes: Vec<u8>) -> bool {
        self.sender.send(bytes).await.is_ok()
    }

    /// Send a line of text with CRLF, IAC-escaping any `0xFF` bytes.
    pub async fn send_line(&self, line: &str) -> bool {
        let mut buf = Vec::with_capacity(line.len() + 2);
        for &b in line.as_bytes() {
            if b == 0xFF {
                buf.push(0xFF);
            }
            buf.push(b);
        }
        buf.extend_from_slice(b"\r\n");
        self.sender.send(buf).await.is_ok()
    }
}

// ── File loader ───────────────────────────────────────────────────────────

/// Expand a leading `~` to `$HOME`.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}{rest}")
    } else {
        path.to_owned()
    }
}

/// Build the [`FileLoader`] callback used by the interpreter for `/load`.
fn make_file_loader() -> FileLoader {
    use std::sync::Arc;
    Arc::new(|path: &str| {
        let resolved = expand_tilde(path);
        // Try filesystem first (covers both explicit paths and user-installed overrides).
        if let Ok(src) = std::fs::read_to_string(&resolved) {
            return Ok(src);
        }
        // Fall back to embedded registry by basename.
        let name = std::path::Path::new(&resolved)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path);
        crate::embedded::get_embedded(name)
            .map(|s| s.to_owned())
            .ok_or_else(|| format!("{resolved}: not found"))
    })
}

// ── StatusField ───────────────────────────────────────────────────────────────

/// One named column in the status bar.
#[derive(Debug, Clone)]
pub struct StatusField {
    /// Column name (without the `@` prefix).  Empty = spacer.
    pub name: String,
    /// `true` → dynamic: expression re-evaluated on every display refresh.
    /// `false` → static: value read from `status_var_<name>` global.
    pub dynamic: bool,
    /// Explicit column width in characters.  `None` = auto-width.
    pub width: Option<usize>,
    /// Optional label string (stored for serialisation; not yet rendered).
    pub label: Option<String>,
}

impl StatusField {
    /// Serialize back to the spec token form `[@]name[:width[:label]]`.
    fn to_spec(&self) -> String {
        let prefix = if self.dynamic { "@" } else { "" };
        let mut s = format!("{}{}", prefix, self.name);
        if let Some(w) = self.width {
            s.push_str(&format!(":{w}"));
            if let Some(ref lbl) = self.label {
                s.push(':');
                s.push_str(lbl);
            }
        }
        s
    }
}

/// Parse one field spec token: `[@]name[:width[:label]]`.
///
/// Width is the leading run of digits before any trailing flag chars such as
/// `P3` — those flags are silently ignored.
fn parse_field_spec(token: &str) -> StatusField {
    let (dynamic, rest) = token.strip_prefix('@')
        .map(|r| (true, r))
        .unwrap_or((false, token));
    let mut parts = rest.splitn(3, ':');
    let name = parts.next().unwrap_or("").to_owned();
    let width = parts.next().and_then(|s| {
        // Accept leading digits only (ignore trailing flag chars like "P3").
        let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<usize>().ok()
    });
    let label = parts.next().map(|s| s.to_owned());
    StatusField { name, dynamic, width, label }
}

/// Parse the full argument string of `/status_add` into a list of fields.
///
/// Bare `-` tokens (positional separators) are skipped.
fn parse_field_list(raw: &str) -> Vec<StatusField> {
    raw.split_whitespace()
        .filter(|t| *t != "-")
        .map(parse_field_spec)
        .collect()
}

// ── EventLoop ─────────────────────────────────────────────────────────────

/// The top-level runtime: ties together all subsystems and drives them from
/// a single `tokio::select!` loop.
///
/// Create with [`EventLoop::new`], optionally call [`EventLoop::connect`] to
/// open MUD connections, then call [`EventLoop::run`].
pub struct EventLoop {
    /// Inbound events from all connection tasks.
    net_rx: mpsc::Receiver<NetMessage>,
    net_tx: mpsc::Sender<NetMessage>,
    /// Outbound handles — one per live connection.
    handles: HashMap<String, ConnectionHandle>,
    /// Connection names in the order they were opened (used by `/fg -c<n>`).
    world_order: Vec<String>,
    /// Which world the user is currently typing into.
    active_world: Option<String>,

    input: InputProcessor,
    key_decoder: KeyDecoder,
    terminal: Terminal,
    worlds: WorldStore,
    scheduler: ProcessScheduler,

    /// Script interpreter — executes user commands and loaded `.tf` files.
    pub interp: Interpreter,

    /// Macro / trigger / hook store.
    macro_store: MacroStore,

    /// Output scrollback buffer.
    screen: Screen,

    /// Status bar content.
    status: StatusLine,

    /// Set to `true` to exit the main loop after the current iteration.
    quit: bool,

    /// Path to check for new mail (mirrors `%mailpath`).
    mail_path: Option<PathBuf>,
    mail_next: Instant,

    /// True when the screen needs a full redraw.
    need_refresh: bool,

    /// Open log file (mirrors `/log path`).
    log_file: Option<tokio::fs::File>,

    /// Format string for the status bar.  Tokens: `%world`, `%T` (HH:MM),
    /// `%t` (HH:MM:SS).  Defaults to `"[ %world ]  %T"`.
    status_format: String,

    /// When the user last pressed a key (for `idle()`).
    last_keystroke: Instant,
    /// When data was last received from any server (for `sidle()`).
    last_server_data: Instant,

    /// Named status-bar fields populated by `/status_add`.
    /// When non-empty, these replace the simple `%world`/`%T` token format.
    status_fields: Vec<StatusField>,

    /// Watchdog: reconnect if this world goes silent for `watchdog_interval`.
    watchdog_interval: Option<Duration>,
    /// Which world the watchdog monitors (`None` = active world).
    watchdog_world: Option<String>,
    /// Per-world timestamp of the last received line (used by watchdog check).
    last_data_per_world: HashMap<String, Instant>,

    /// `-l` flag: suppress the LOGIN hook on auto-connect (mirrors C TF `CONN_AUTOLOGIN`).
    pub no_autologin: bool,
    /// `-q` flag: quiet login — suppress initial server output after auto-connect.
    /// Sets `%quiet=1` in the interpreter; `numquiet` line-suppression not yet implemented.
    pub quiet_login: bool,

    /// Prompt string set by the `prompt()` TF function or by server PROMPT events.
    /// Displayed to the left of the editable input buffer (not editable itself).
    /// Mirrors C TF's `sock->prompt` / `update_prompt()`.
    input_prompt: String,
}

impl EventLoop {
    /// Create a new, idle event loop (no connections open yet).
    pub fn new() -> Self {
        let (net_tx, net_rx) = mpsc::channel(256);
        let terminal = Terminal::new(std::io::stdout())
            .expect("failed to create terminal");
        let screen = Screen::new(terminal.width as usize,
                                 terminal.output_bottom() as usize);
        let mut interp = Interpreter::new();
        interp.file_loader = Some(make_file_loader());
        Self {
            net_rx,
            net_tx,
            handles: HashMap::new(),
            world_order: Vec::new(),
            active_world: None,
            input: InputProcessor::new(500),
            key_decoder: KeyDecoder::new(),
            terminal,
            worlds: WorldStore::new(),
            scheduler: ProcessScheduler::new(),
            interp,
            macro_store: MacroStore::new(),
            screen,
            status: StatusLine::default(),
            quit: false,
            mail_path: None,
            mail_next: Instant::now() + MAIL_CHECK_INTERVAL,
            status_format: "[ %world ]  %T".to_owned(),
            need_refresh: false,
            log_file: None,
            last_keystroke: Instant::now(),
            last_server_data: Instant::now(),
            status_fields: Vec::new(),
            watchdog_interval: None,
            watchdog_world: None,
            last_data_per_world: HashMap::new(),
            no_autologin: false,
            quiet_login: false,
            input_prompt: String::new(),
        }
    }

    /// Returns `true` if there is a default world to auto-connect to.
    pub fn has_default_world(&self) -> bool {
        self.worlds.default_world().is_some()
    }

    /// Push a plain text line into the scrollback buffer.
    ///
    /// Used by `main.rs` to emit startup banner messages before `run()`.
    pub fn push_output(&mut self, line: &str) {
        self.screen.push_line(LogicalLine::plain(line));
    }

    /// Execute a `.tf` script file through the interpreter.
    ///
    /// Output lines are printed immediately; [`ScriptAction`]s that make sense
    /// during startup (e.g. `AddWorld`) are processed; others are dropped.
    /// Returns `Err` if the file cannot be read.
    pub fn load_script_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.load_script_source(&src, &path.display().to_string())
    }

    /// Execute a TF script given its source directly (e.g. from embedded files).
    pub fn load_script_source(&mut self, src: &str, label: &str) -> Result<(), String> {
        self.screen.push_line(LogicalLine::plain(&format!("% Loading commands from {label}.")));
        self.interp.file_load_mode = true;
        let r = self.interp.exec_script(src);
        self.interp.file_load_mode = false;
        r.map_err(|e| format!("{label}: {e}"))?;
        // Drain interpreter output into the screen buffer so it appears in
        // the TUI window (matching C TF's behaviour where all output is in-window).
        for line in self.interp.output.drain(..) {
            self.screen.push_line(LogicalLine::plain(&line));
        }
        // Process startup-safe actions.
        for action in self.interp.take_actions() {
            // Quit/Connect/Disconnect are deferred until after startup.
            match action {
                ScriptAction::AddWorld(w) => { self.worlds.upsert(w); }
                ScriptAction::DefMacro(mac) => {
                    if let Some(n) = &mac.name { self.interp.macro_names.insert(n.clone()); }
                    self.macro_store.add(mac);
                }
                // Status bar configuration is safe to apply at startup.
                ScriptAction::StatusAdd { clear, raw } => {
                    if clear { self.status_fields.clear(); }
                    let new_fields = parse_field_list(&raw);
                    self.status_fields.extend(new_fields);
                    self.sync_status_fields_global();
                }
                ScriptAction::StatusRm(name) => {
                    self.status_fields.retain(|f| f.name != name);
                    self.sync_status_fields_global();
                }
                ScriptAction::StatusEdit { name, raw } => {
                    if let Some(f) = self.status_fields.iter_mut().find(|f| f.name == name) {
                        *f = parse_field_spec(&raw);
                    }
                    self.sync_status_fields_global();
                }
                ScriptAction::StatusClear => {
                    self.status_fields.clear();
                    self.sync_status_fields_global();
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Open a plain-TCP connection to `host:port` and register it as `world_name`.
    pub async fn connect(&mut self, world_name: &str, host: &str, port: u16) -> io::Result<()> {
        let conn = Connection::connect_plain(host, port).await?;
        self.register(world_name, conn);
        Ok(())
    }

    /// Open a TLS connection and register it as `world_name`.
    pub async fn connect_tls(&mut self, world_name: &str, host: &str, port: u16) -> io::Result<()> {
        let conn = Connection::connect_tls(host, port).await?;
        self.register(world_name, conn);
        Ok(())
    }

    fn register(&mut self, world_name: &str, conn: Connection) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<ToServer>(64);
        let event_tx = self.net_tx.clone();
        let name = world_name.to_owned();
        tokio::spawn(connection_task(conn, cmd_rx, event_tx, name.clone()));
        let handle = ConnectionHandle { world_name: name.clone(), sender: cmd_tx };
        self.handles.insert(name.clone(), handle);
        if !self.world_order.contains(&name) {
            self.world_order.push(name.clone());
        }
        if self.active_world.is_none() {
            self.active_world = Some(name);
        }
    }

    /// Send a line to the active world connection.
    pub async fn send_to_active(&self, line: &str) -> bool {
        if let Some(world) = &self.active_world {
            if let Some(handle) = self.handles.get(world) {
                return handle.send_line(line).await;
            }
        }
        false
    }

    // ── Main loop ─────────────────────────────────────────────────────────

    /// Run the event loop until shutdown is requested.
    ///
    /// Installs signal handlers for SIGWINCH, SIGTERM, and SIGINT, then
    /// drives keyboard input, socket events, timers, and scheduled processes
    /// in a single `tokio::select!` loop.
    pub async fn run(&mut self) -> io::Result<()> {
        let mut sigwinch = signal(SignalKind::window_change())?;
        let mut sigterm  = signal(SignalKind::terminate())?;
        let mut sigint   = signal(SignalKind::interrupt())?;
        let mut sighup   = signal(SignalKind::hangup())?;

        // Enable raw mode for the duration of the session.
        let _raw = Terminal::enter_raw_mode()?;

        // Initial full-screen paint.
        self.refresh_display();

        // Spawn a dedicated stdin-reading thread.
        //
        // tokio::io::stdin() uses spawn_blocking under the hood.  When
        // select! drops the future (to handle another branch), the blocking
        // thread is orphaned but still running — on the next iteration a new
        // thread is spawned, so eventually many threads compete to read the
        // same stdin fd, causing lost keystrokes.  A dedicated thread that
        // owns stdin and sends data through an mpsc channel avoids this.
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(16);
        std::thread::spawn(move || {
            use std::io::Read;
            let stdin = std::io::stdin();
            let mut guard = stdin.lock();
            let mut buf = [0u8; 256];
            loop {
                match guard.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        // EOF or error — send an empty vec as sentinel.
                        let _ = stdin_tx.blocking_send(vec![]);
                        break;
                    }
                    Ok(n) => {
                        if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break; // receiver dropped (event loop exited)
                        }
                    }
                }
            }
        });

        while !self.quit {
            // ── Compute the next timer deadline ──────────────────────────
            let now = Instant::now();
            let deadline = [
                self.scheduler.next_wakeup(),
                Some(self.mail_next),
                Some(now + REFRESH_INTERVAL),
                self.watchdog_deadline(),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + Duration::from_secs(3600));

            let timer = sleep_until(deadline.into());
            tokio::pin!(timer);

            // ── Select ───────────────────────────────────────────────────
            tokio::select! {
                // Keyboard input (from dedicated stdin-reading thread).
                Some(bytes) = stdin_rx.recv() => {
                    if bytes.is_empty() {
                        // EOF or error sentinel from the stdin thread.
                        self.quit = true;
                    } else {
                        self.last_keystroke = Instant::now();
                        for b in bytes {
                            if let Some(action) = self.key_decoder.push(b) {
                                // Intercept scrollback ops before the editor.
                                match &action {
                                    EditAction::Bound(KeyBinding::DoKey(DoKeyOp::ScrollUp)) => {
                                        let page = self.terminal.output_bottom() as usize;
                                        self.screen.scroll_up(page);
                                        self.need_refresh = true;
                                    }
                                    EditAction::Bound(KeyBinding::DoKey(DoKeyOp::ScrollDown)) => {
                                        let page = self.terminal.output_bottom() as usize;
                                        self.screen.scroll_down(page);
                                        self.need_refresh = true;
                                    }
                                    EditAction::Bound(KeyBinding::Macro(body)) => {
                                        let body = body.clone();
                                        self.run_command(&body).await;
                                    }
                                    _ => {
                                        if let Some(line) = self.input.apply(action) {
                                            self.dispatch_line(line).await;
                                        }
                                    }
                                }
                            }
                        }
                        // If the command produced output, do a full refresh
                        // immediately rather than waiting for the timer tick.
                        if self.need_refresh {
                            self.refresh_display();
                            self.need_refresh = false;
                        } else {
                            // No output — just redraw the (possibly cleared)
                            // input line so the user sees their typing.
                            self.sync_kb_globals();
                            let pos  = self.input.editor.pos;
                            let text = format!("{}{}", self.input_prompt, self.input.editor.text());
                            let _ = self.terminal.render_input(&text, pos + self.input_prompt.chars().count());
                            let _ = self.terminal.flush();
                        }
                    }
                }

                // Events from any connection task.
                Some(msg) = self.net_rx.recv() => {
                    self.handle_net_message(msg).await;
                }

                // Terminal resize.
                _ = sigwinch.recv() => {
                    if let Ok((w, h)) = crossterm::terminal::size() {
                        self.terminal.handle_resize(w, h);
                        self.screen.resize(w as usize, self.terminal.output_bottom() as usize);
                        self.need_refresh = true;
                    }
                }

                // Graceful shutdown.
                _ = sigterm.recv() => {
                    self.fire_hook(Hook::SigTerm, "").await;
                    self.quit = true;
                }
                _ = sigint.recv()  => self.quit = true,
                _ = sighup.recv()  => {
                    self.fire_hook(Hook::SigHup, "").await;
                    self.quit = true;
                }

                // Timer tick.
                _ = &mut timer => {
                    let now = Instant::now();
                    self.run_due_processes(now).await;
                    self.check_mail(now).await;
                    self.check_watchdog(now).await;
                    if self.need_refresh {
                        self.refresh_display();
                        self.need_refresh = false;
                    }
                }
            }
        }

        // Clear the status bar and input line before leaving raw mode so the
        // shell prompt appears cleanly below the last line of session output.
        self.terminal.cleanup();
        self.shutdown();
        Ok(())
    }

    // ── Input dispatch ────────────────────────────────────────────────────

    async fn dispatch_line(&mut self, line: String) {
        if line.starts_with('/') {
            self.run_command(&line).await;
        } else {
            self.fire_hook_sync(Hook::Send, &line);
            self.send_to_active(&line).await;
        }
    }

    async fn run_command(&mut self, cmd: &str) {
        if let Err(e) = self.interp.exec_script(cmd) {
            self.screen.push_line(LogicalLine::plain(&format!("% Error: {e}")));
            self.need_refresh = true;
        }
        // Push interpreter output to the screen, parsing @{...} markup.
        let lines: Vec<String> = self.interp.output.drain(..).collect();
        for line in lines {
            let content = crate::tfstr::TfStr::from_tf_markup(&line);
            self.screen.push_line(LogicalLine::new(content, Attr::EMPTY));
            self.need_refresh = true;
        }
        // Process queued side-effects.
        let actions: Vec<ScriptAction> = self.interp.take_actions();
        for action in actions {
            self.handle_script_action(action).await;
        }
    }

    async fn handle_script_action(&mut self, action: ScriptAction) {
        match action {
            ScriptAction::Quit => self.quit = true,

            ScriptAction::SendToWorld { text, world, no_newline } => {
                let name = world.or_else(|| self.active_world.clone());
                if let Some(n) = name {
                    if let Some(h) = self.handles.get(&n) {
                        if no_newline {
                            h.send_raw(text.into_bytes()).await;
                        } else {
                            h.send_line(&text).await;
                        }
                    }
                }
            }

            ScriptAction::Connect { name } => {
                self.connect_world_by_name(&name).await;
            }

            ScriptAction::Disconnect { name } => {
                let target = if name.is_empty() {
                    self.active_world.clone()
                } else {
                    Some(name)
                };
                if let Some(n) = target {
                    self.handles.remove(&n);
                    self.world_order.retain(|w| w != &n);
                    if self.active_world.as_deref() == Some(&n) {
                        self.active_world = self.world_order.first().cloned();
                    }
                    let msg = format!("** Disconnected from {n} **");
                    self.screen.push_line(LogicalLine::plain(&msg));
                    self.fire_hook_sync(Hook::Disconnect, &n);
                    self.update_status();
                    self.need_refresh = true;
                }
            }

            ScriptAction::AddWorld(w) => {
                self.worlds.upsert(w);
            }

            ScriptAction::DefMacro(mac) => {
                if let Some(n) = &mac.name { self.interp.macro_names.insert(n.clone()); }
                let _ = self.macro_store.add(mac);
            }

            ScriptAction::SwitchWorld { name } => {
                if self.handles.contains_key(&name) {
                    self.active_world = Some(name.clone());
                    self.update_status();
                    // Fire WORLD hook and push divider (mirrors C TF world_hook()).
                    let world_msg = format!("---- World {name} ----");
                    self.screen.push_line(LogicalLine::plain(&world_msg));
                    self.fire_hook(Hook::World, &world_msg).await;
                    self.need_refresh = true;
                } else {
                    let msg = format!("% No open connection to '{name}'");
                    self.screen.push_line(LogicalLine::plain(&msg));
                    self.need_refresh = true;
                }
            }

            ScriptAction::FgWorld { index, quiet } => {
                // /fg -c<n>: switch active world by 1-based connection index.
                // /fg -n   : foreground "no world" (detach from all connections).
                let next = if let Some(n) = index {
                    // Negative n counts from end; 0 → last world.
                    let idx = if n <= 0 {
                        (self.world_order.len() as i64 + n).max(0) as usize
                    } else {
                        (n - 1).max(0) as usize
                    };
                    self.world_order.get(idx).cloned()
                } else {
                    None // /fg -n
                };
                self.active_world = next.clone();
                if !quiet {
                    let label = next.as_deref().unwrap_or("none");
                    let world_msg = format!("---- World {label} ----");
                    self.screen.push_line(LogicalLine::plain(&world_msg));
                    self.fire_hook(Hook::World, &world_msg).await;
                }
                self.update_status();
                self.need_refresh = true;
            }

            // ── Process scheduling ─────────────────────────────────────────

            ScriptAction::AddRepeat { interval_ms, count, body, world } => {
                let interval = Duration::from_millis(interval_ms);
                self.scheduler.add_repeat(body, interval, count, world);
            }

            ScriptAction::AddQuoteFile { interval_ms, path, world } => {
                let interval = Duration::from_millis(interval_ms);
                self.scheduler.add_quote_file(PathBuf::from(path), interval, None, world);
            }

            ScriptAction::AddQuoteShell { interval_ms, command, world } => {
                let interval = Duration::from_millis(interval_ms);
                self.scheduler.add_quote_shell(command, interval, None, world);
            }

            ScriptAction::QuoteFileSync { path, world } => {
                // Send all lines from the file immediately, no scheduling delay.
                let target = world.or_else(|| self.active_world.clone());
                if let Some(ref w) = target {
                    if let Ok(contents) = tokio::fs::read_to_string(&path).await {
                        let world_name = w.clone();
                        for line in contents.lines() {
                            if let Some(handle) = self.handles.get(&world_name) {
                                handle.send_line(line).await;
                            }
                        }
                    }
                }
            }

            ScriptAction::QuoteShellSync { command, world } => {
                // Run shell command and send all output lines immediately.
                let target = world.or_else(|| self.active_world.clone());
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .output()
                    .await;
                if let Ok(out) = output {
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    if let Some(ref w) = target {
                        for line in text.lines() {
                            if let Some(handle) = self.handles.get(w) {
                                handle.send_line(line).await;
                            }
                        }
                    }
                }
            }

            // ── Macro / binding management ────────────────────────────────

            ScriptAction::UndefMacro(name) => {
                self.macro_store.remove_by_name(&name);
            }

            ScriptAction::UnbindKey(seq) => {
                let bytes = crate::keybind::key_sequence(&seq);
                self.key_decoder.keymap.unbind(&bytes);
            }

            // ── Introspection ─────────────────────────────────────────────

            ScriptAction::ListWorlds => {
                let lines: Vec<String> = self.worlds.iter()
                    .map(|w| format!("  {:<20} {}:{}",
                        w.name,
                        w.host.as_deref().unwrap_or("-"),
                        w.port.as_deref().unwrap_or("23")))
                    .collect();
                if lines.is_empty() {
                    self.screen.push_line(LogicalLine::plain("% No worlds defined."));
                } else {
                    self.screen.push_line(LogicalLine::plain("% Worlds:"));
                    for line in lines {
                        self.screen.push_line(LogicalLine::plain(&line));
                    }
                }
                self.need_refresh = true;
            }

            ScriptAction::ListMacros { filter } => {
                let lines: Vec<String> = self.macro_store.iter()
                    .filter(|m| match &filter {
                        Some(f) => m.name.as_deref().is_some_and(|n| n.starts_with(f.as_str())),
                        None => true,
                    })
                    .map(|m| format!("  /def {} = {}",
                        m.name.as_deref().unwrap_or("(unnamed)"),
                        m.body.as_deref().unwrap_or("")))
                    .collect();
                if lines.is_empty() {
                    self.screen.push_line(LogicalLine::plain("% No macros defined."));
                } else {
                    for line in lines {
                        self.screen.push_line(LogicalLine::plain(&line));
                    }
                }
                self.need_refresh = true;
            }

            // ── Session logging ───────────────────────────────────────────

            ScriptAction::StartLog(path) => {
                match tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await {
                    Ok(file) => {
                        self.log_file = Some(file);
                        let msg = format!("% Logging to {path}");
                        self.screen.push_line(LogicalLine::plain(&msg));
                    }
                    Err(e) => {
                        self.screen.push_line(LogicalLine::plain(&format!("% /log: {e}")));
                    }
                }
                self.need_refresh = true;
            }

            ScriptAction::StopLog => {
                if self.log_file.take().is_some() {
                    self.screen.push_line(LogicalLine::plain("% Logging stopped."));
                }
                self.need_refresh = true;
            }

            // ── Lua scripting ─────────────────────────────────────────────

            #[cfg(feature = "lua")]
            ScriptAction::LuaLoad(_path) => {
                self.screen.push_line(LogicalLine::plain(
                    "% /loadlua: Lua engine not yet connected to event loop"));
                self.need_refresh = true;
            }

            #[cfg(feature = "lua")]
            ScriptAction::LuaCall { .. } => {
                self.screen.push_line(LogicalLine::plain(
                    "% /calllua: Lua engine not yet connected to event loop"));
                self.need_refresh = true;
            }

            #[cfg(feature = "lua")]
            ScriptAction::LuaPurge => {}

            // ── Python scripting ──────────────────────────────────────────

            #[cfg(feature = "python")]
            ScriptAction::PythonExec(_) => {
                self.screen.push_line(LogicalLine::plain(
                    "% /python: Python engine not yet connected to event loop"));
                self.need_refresh = true;
            }

            #[cfg(feature = "python")]
            ScriptAction::PythonCall { .. } => {
                self.screen.push_line(LogicalLine::plain(
                    "% /callpython: Python engine not yet connected to event loop"));
                self.need_refresh = true;
            }

            #[cfg(feature = "python")]
            ScriptAction::PythonLoad(_) => {
                self.screen.push_line(LogicalLine::plain(
                    "% /loadpython: Python engine not yet connected to event loop"));
                self.need_refresh = true;
            }

            #[cfg(feature = "python")]
            ScriptAction::PythonKill => {}

            // ── Miscellaneous ─────────────────────────────────────────────

            ScriptAction::SaveWorlds { path, name } => {
                let lines: Vec<String> = self
                    .worlds
                    .iter()
                    .filter(|w| name.as_deref().is_none_or(|n| w.name == n))
                    .map(|w| w.to_addworld())
                    .collect();
                match path {
                    Some(p) => {
                        let content = lines.join("\n") + "\n";
                        match tokio::fs::write(&p, &content).await {
                            Ok(()) => {
                                let msg = format!("% Saved {} world(s) to {p}", lines.len());
                                self.screen.push_line(LogicalLine::plain(&msg));
                            }
                            Err(e) => {
                                self.screen.push_line(LogicalLine::plain(
                                    &format!("% /saveworld: {e}")));
                            }
                        }
                    }
                    None => {
                        for line in &lines {
                            self.screen.push_line(LogicalLine::plain(line));
                        }
                        if lines.is_empty() {
                            self.screen.push_line(LogicalLine::plain("% No worlds to save."));
                        }
                    }
                }
                self.need_refresh = true;
            }

            ScriptAction::Bell => {
                use std::io::Write;
                let _ = std::io::stdout().write_all(b"\x07");
                let _ = std::io::stdout().flush();
            }

            ScriptAction::PurgeMacros(pattern) => {
                let count = self.macro_store.purge(pattern.as_deref());
                let msg = format!("% Purged {count} macro(s).");
                self.screen.push_line(LogicalLine::plain(&msg));
                self.need_refresh = true;
            }

            ScriptAction::SetInput(text) => {
                self.input.editor.set_text(&text);
                self.need_refresh = true;
            }

            ScriptAction::RecordLine(line) => {
                self.input.history.record(&line);
            }

            ScriptAction::SetWatchdog(secs) => {
                self.watchdog_interval = if secs == 0 {
                    None
                } else {
                    Some(Duration::from_secs(secs))
                };
            }

            ScriptAction::SetWatchName(name) => {
                self.watchdog_world = if name.is_empty() { None } else { Some(name) };
            }

            ScriptAction::Suspend => {
                // Leave raw mode, stop the process, re-enter raw mode on resume.
                crossterm::terminal::disable_raw_mode().ok();
                // SAFETY: raise(3) is async-signal-safe; SIGSTOP cannot be caught.
                unsafe { libc::raise(libc::SIGSTOP); }
                // We resume here after SIGCONT.
                crossterm::terminal::enable_raw_mode().ok();
                self.refresh_display();
            }

            ScriptAction::Option102 { data, world } => {
                let target = world.or_else(|| self.active_world.clone());
                if let Some(w) = target {
                    if let Some(handle) = self.handles.get(&w) {
                        let mut pkt = vec![
                            crate::telnet::IAC,
                            crate::telnet::SB,
                            crate::telnet::opt::OPT102,
                        ];
                        pkt.extend_from_slice(&data);
                        pkt.extend_from_slice(&[crate::telnet::IAC, crate::telnet::SE]);
                        handle.send_raw(pkt).await;
                    }
                }
            }

            ScriptAction::SetStatus(fmt) => {
                self.status_format = fmt;
                self.update_status();
                self.need_refresh = true;
            }

            ScriptAction::DoKey(op) => {
                use crate::keybind::{EditAction, KeyBinding};
                let action = EditAction::Bound(KeyBinding::DoKey(op));
                // Apply the editor operation; discard any submitted line —
                // dispatching from inside an action handler would cause async
                // re-entrancy.  Scripts rarely (if ever) call /dokey NEWLINE.
                let _ = self.input.apply(action);
                self.need_refresh = true;
            }

            ScriptAction::ShellCmd(cmd) => {
                // Run `sh -c <cmd>`, display stdout+stderr on the TF screen.
                let result = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .output()
                    .await;
                match result {
                    Ok(out) => {
                        let combined = [out.stdout.as_slice(), out.stderr.as_slice()].concat();
                        let text = String::from_utf8_lossy(&combined);
                        for line in text.lines() {
                            self.screen.push_line(LogicalLine::plain(line));
                        }
                    }
                    Err(e) => {
                        let msg = format!("% /sh: {e}");
                        self.screen.push_line(LogicalLine::plain(&msg));
                    }
                }
                self.need_refresh = true;
            }

            ScriptAction::ShellInteractive => {
                // Drop to an interactive shell: leave raw mode, run $SHELL,
                // re-enter raw mode and repaint when it exits.
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
                crossterm::terminal::disable_raw_mode().ok();
                let _ = tokio::process::Command::new(&shell).status().await;
                crossterm::terminal::enable_raw_mode().ok();
                self.need_refresh = true;
            }

            ScriptAction::EditInput => {
                // Open the current input buffer in $EDITOR, then re-insert the
                // result (mirrors C TF's handle_edit_command in command.c).
                let editor = std::env::var("EDITOR")
                    .or_else(|_| std::env::var("VISUAL"))
                    .unwrap_or_else(|_| "vi".to_owned());
                let current_text = self.input.editor.text();
                // Use a securely-created temp file (avoids predictable-path symlink attacks).
                if let Ok(tmp) = tempfile::NamedTempFile::new() {
                    let tmp_path = tmp.path().to_owned();
                    if tokio::fs::write(&tmp_path, &current_text).await.is_ok() {
                        crossterm::terminal::disable_raw_mode().ok();
                        let _ = tokio::process::Command::new(&editor).arg(&tmp_path).status().await;
                        crossterm::terminal::enable_raw_mode().ok();
                        // Read back and re-insert (strip trailing newline).
                        if let Ok(mut edited) = tokio::fs::read_to_string(&tmp_path).await {
                            if edited.ends_with('\n') { edited.pop(); }
                            if edited.ends_with('\r') { edited.pop(); }
                            self.input.editor.set_text(&edited);
                        }
                    }
                    // tmp is dropped here — NamedTempFile deletes the file on drop.
                }
                self.need_refresh = true;
            }

            ScriptAction::Recall(n) => {
                let entries: Vec<String> = self.input.history.iter_oldest_first()
                    .map(|s| s.to_owned())
                    .collect();
                let total = entries.len();
                let skip = n.map(|n| total.saturating_sub(n)).unwrap_or(0);
                for (i, entry) in entries.iter().enumerate().skip(skip) {
                    let line = format!("[{}] {}", i + 1, entry);
                    self.screen.push_line(LogicalLine::plain(&line));
                }
                self.need_refresh = true;
            }

            ScriptAction::UnWorld(name) => {
                if self.worlds.remove(&name) {
                    let msg = format!("% World '{name}' removed.");
                    self.screen.push_line(LogicalLine::plain(&msg));
                } else {
                    let msg = format!("% /unworld: no world named '{name}'.");
                    self.screen.push_line(LogicalLine::plain(&msg));
                }
                self.need_refresh = true;
            }

            ScriptAction::SetHistSize(n) => {
                self.screen.max_lines = n;
                let msg = format!("% History size set to {n}.");
                self.screen.push_line(LogicalLine::plain(&msg));
                self.need_refresh = true;
            }

            ScriptAction::SaveMacros { path } => {
                let lines: Vec<String> = self.macro_store.iter()
                    .filter(|m| !m.invisible)
                    .map(|m| m.to_def_command())
                    .collect();
                match path {
                    None => {
                        // No file — print to screen.
                        for line in &lines {
                            self.screen.push_line(LogicalLine::plain(line));
                        }
                    }
                    Some(p) => {
                        let content = lines.join("\n") + "\n";
                        match tokio::fs::write(&p, &content).await {
                            Ok(()) => {
                                let msg = format!("% Saved {} macro(s) to {p}.", lines.len());
                                self.screen.push_line(LogicalLine::plain(&msg));
                            }
                            Err(e) => {
                                let msg = format!("% /save: {p}: {e}");
                                self.screen.push_line(LogicalLine::plain(&msg));
                            }
                        }
                    }
                }
                self.need_refresh = true;
            }

            ScriptAction::ListProcesses => {
                let procs: Vec<_> = self.scheduler.iter().collect();
                if procs.is_empty() {
                    self.screen.push_line(LogicalLine::plain("% No processes running."));
                } else {
                    self.screen.push_line(LogicalLine::plain("% PID  TYPE     INTERVAL  RUNS  DESCRIPTION"));
                    for p in procs {
                        use crate::process::ProcKind;
                        let (kind, desc) = match &p.kind {
                            ProcKind::Repeat { body } => ("repeat", body.as_str()),
                            ProcKind::QuoteFile { path, .. } =>
                                ("quote", path.to_str().unwrap_or("?")),
                            ProcKind::QuoteShell { command } => ("quote!", command.as_str()),
                        };
                        let runs = p.runs_left.map_or("∞".to_owned(), |n| n.to_string());
                        let interval_ms = p.interval.as_millis();
                        let line = format!(
                            "% {:<4} {:<8} {:>6}ms  {:>4}  {}",
                            p.id, kind, interval_ms, runs, desc
                        );
                        self.screen.push_line(LogicalLine::plain(&line));
                    }
                }
                self.need_refresh = true;
            }

            ScriptAction::KillProcess(id) => {
                if self.scheduler.remove(id) {
                    let msg = format!("% Process {id} killed.");
                    self.screen.push_line(LogicalLine::plain(&msg));
                } else {
                    let msg = format!("% /kill: no process with id {id}.");
                    self.screen.push_line(LogicalLine::plain(&msg));
                }
                self.need_refresh = true;
            }

            ScriptAction::FakeRecv { world, line } => {
                // Inject a synthetic received line.  We process it inline
                // (via fire_hook_sync) rather than calling handle_net_message
                // to avoid an async recursion cycle.
                let world_name = world.unwrap_or_else(|| {
                    self.active_world.clone().unwrap_or_default()
                });
                let is_active = self.active_world.as_deref() == Some(world_name.as_str());

                // Trigger matching.
                let actions = self.macro_store.find_triggers(&line, Some(world_name.as_str()));
                let gagged = actions.iter().any(|a| a.gag);
                let merged_attr = actions.iter().fold(Attr::EMPTY, |acc, a| acc | a.attr);

                if !gagged {
                    let ll = if merged_attr == Attr::EMPTY {
                        LogicalLine::plain(&line)
                    } else {
                        LogicalLine::new(
                            { let mut t = crate::tfstr::TfStr::new(); t.push_str(&line, None); t },
                            merged_attr,
                        )
                    };
                    self.screen.push_line(ll);
                    if let Some(ref mut f) = self.log_file {
                        use tokio::io::AsyncWriteExt;
                        let _ = f.write_all(format!("{line}\n").as_bytes()).await;
                    }
                }

                // Execute trigger bodies inline.
                for ta in &actions {
                    if let Some(body) = &ta.body {
                        if let Err(e) = self.interp.exec_script(body) {
                            let m = format!("% Trigger error: {e}");
                            self.screen.push_line(LogicalLine::plain(&m));
                        }
                        for out in self.interp.output.drain(..) {
                            let content = crate::tfstr::TfStr::from_tf_markup(&out);
                            self.screen.push_line(LogicalLine::new(content, Attr::EMPTY));
                        }
                    }
                }

                let hook = if is_active { Hook::Activity } else { Hook::BgText };
                self.fire_hook_sync(hook, &line);
                self.need_refresh = true;
            }

            ScriptAction::LocalLine(msg) => {
                self.screen.push_line(LogicalLine::plain(&msg));
                self.need_refresh = true;
            }

            ScriptAction::UndefMacrosMatching(pat) => {
                // Bulk-remove macros whose name contains `pat` (substring match).
                let names: Vec<String> = self.macro_store.iter()
                    .filter(|m| m.name.as_deref().map(|n| n.contains(pat.as_str())).unwrap_or(false))
                    .filter_map(|m| m.name.clone())
                    .collect();
                for n in names {
                    self.macro_store.remove_by_name(&n);
                }
                self.need_refresh = true;
            }

            ScriptAction::MoreScroll(n) => {
                if n < 0 {
                    for _ in 0..(-n) { self.screen.scroll_up(1); }
                } else {
                    for _ in 0..n { self.screen.scroll_down(1); }
                }
                self.need_refresh = true;
            }

            ScriptAction::KbDelTo(target) => {
                let cursor = self.input.editor.pos;
                let len = self.input.editor.len();
                let target = (target.max(0) as usize).min(len);
                let (start, end) = if target < cursor {
                    (target, cursor)
                } else {
                    (cursor, target)
                };
                if start < end {
                    self.input.editor.delete_region(start, end);
                }
                self.need_refresh = true;
            }

            ScriptAction::KbGoto(pos) => {
                self.input.editor.move_to(pos);
                self.need_refresh = true;
            }

            // ── Status bar field management ────────────────────────────────────
            ScriptAction::StatusAdd { clear, raw } => {
                if clear {
                    self.status_fields.clear();
                }
                let new_fields = parse_field_list(&raw);
                self.status_fields.extend(new_fields);
                self.sync_status_fields_global();
                self.need_refresh = true;
            }

            ScriptAction::StatusRm(name) => {
                self.status_fields.retain(|f| f.name != name);
                self.sync_status_fields_global();
                self.need_refresh = true;
            }

            ScriptAction::StatusEdit { name, raw } => {
                // Parse the new spec to get updated width and label.
                let new = parse_field_spec(&raw);
                if let Some(f) = self.status_fields.iter_mut().find(|f| f.name == name) {
                    if new.width.is_some() { f.width = new.width; }
                    if new.label.is_some() { f.label = new.label.clone(); }
                }
                self.sync_status_fields_global();
                self.need_refresh = true;
            }

            ScriptAction::StatusClear => {
                self.status_fields.clear();
                self.sync_status_fields_global();
                self.need_refresh = true;
            }

            ScriptAction::FireHook { hook, args } => {
                // /trigger -hHOOK args — fire a hook directly at runtime.
                // Used by stdlib.tf's proxy_command after sending the proxy connect string.
                self.fire_hook(hook, &args).await;
            }

            ScriptAction::SetPrompt(text) => {
                // prompt(text) — set the string shown left of the editable input buffer.
                self.input_prompt = text;
                self.need_refresh = true;
            }
        }
    }

    /// Connect to a world by name (looks it up in `WorldStore`).
    /// An empty name means the default world.
    pub async fn connect_world_by_name(&mut self, name: &str) {
        let world = if name.is_empty() {
            // Auto-connect: try the world named "default" first, then fall
            // back to the first defined world (mirrors C TF socket.c behaviour).
            self.worlds.default_world().cloned()
                .or_else(|| self.worlds.iter().next().cloned())
        } else {
            self.worlds.find(name).cloned()
        };
        let Some(w) = world else {
            if name.is_empty() {
                // No worlds defined: fire the World hook (updates status bar)
                // but do not print a screen line — C TF does not do so either.
                self.fire_hook(Hook::World, "").await;
            } else {
                let msg = format!("% Unknown world '{name}'");
                self.screen.push_line(LogicalLine::plain(&msg));
            }
            self.need_refresh = true;
            return;
        };
        let Some(host) = w.host.as_deref() else {
            let msg = format!("% World '{}' has no host", w.name);
            self.screen.push_line(LogicalLine::plain(&msg));
            self.need_refresh = true;
            return;
        };
        let world_host = host.to_owned();
        let world_port_str = w.port.as_deref().unwrap_or("23").to_owned();
        let port: u16 = world_port_str.parse().unwrap_or(23);
        let world_name = w.name.clone();
        let world_char = w.character.clone().unwrap_or_default();
        // Capture credentials before w is consumed by connect().
        let credentials = match (&w.character, &w.pass) {
            (Some(ch), Some(pw)) => Some((ch.clone(), pw.clone())),
            _ => None,
        };

        // Check proxy settings: read global vars before any mutable borrow.
        let proxy_host = self.interp.get_global_var("proxy_host")
            .map(|v| v.to_string())
            .filter(|s| !s.is_empty());
        let use_proxy = proxy_host.is_some() && !w.flags.no_proxy;
        let (connect_host, connect_port): (String, u16) = if use_proxy {
            let ph = proxy_host.unwrap();
            let pp: u16 = self.interp.get_global_var("proxy_port")
                .map(|v| v.to_string())
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse().ok())
                .unwrap_or(23);
            (ph, pp)
        } else {
            (world_host.clone(), port)
        };

        let result = if w.flags.ssl {
            self.connect_tls(&world_name, &connect_host, connect_port).await
        } else {
            self.connect(&world_name, &connect_host, connect_port).await
        };
        match result {
            Ok(()) => {
                // Set world_* variables so hook bodies can expand ${world_host} etc.
                // (Mirrors C TF's world_info() lookup via macro_body() "world_" prefix.)
                self.interp.set_global_var("world_name",      crate::script::Value::Str(world_name.clone()));
                self.interp.set_global_var("world_host",      crate::script::Value::Str(world_host));
                self.interp.set_global_var("world_port",      crate::script::Value::Str(world_port_str));
                self.interp.set_global_var("world_character", crate::script::Value::Str(world_char));
                self.interp.set_global_var("world_login",     crate::script::Value::Int((!self.no_autologin) as i64));

                self.update_status();

                if use_proxy {
                    // Proxy path: fire H_PROXY with the world name.
                    // stdlib.tf's proxy_hook sends "telnet ${world_host} ${world_port}"
                    // to the proxy server, then does /trigger -hCONNECT and -hLOGIN
                    // itself — so we do NOT fire Hook::Connect or Hook::Login here.
                    self.fire_hook(Hook::Proxy, &world_name).await;
                } else {
                    self.fire_hook(Hook::Connect, &world_name).await;
                    // Fire LOGIN hook if autologin is enabled and world has credentials.
                    if !self.no_autologin {
                        if let Some((character, password)) = credentials {
                            let login_args = format!("{world_name} {character} {password}");
                            self.fire_hook(Hook::Login, &login_args).await;
                        }
                    }
                }

                // Fire WORLD hook and push the "---- World X ----" divider
                // (mirrors C TF's world_hook() call in socket.c).
                let world_msg = format!("---- World {world_name} ----");
                self.screen.push_line(LogicalLine::plain(&world_msg));
                self.fire_hook(Hook::World, &world_msg).await;

                self.need_refresh = true;
            }
            Err(e) => {
                let notice = format!("% Connect to '{}' failed: {e}", w.name);
                self.screen.push_line(LogicalLine::plain(&notice));
                self.fire_hook(Hook::ConFail, &w.name).await;
                self.need_refresh = true;
            }
        }
    }

    // ── Net event dispatch ────────────────────────────────────────────────

    pub(crate) async fn handle_net_message(&mut self, msg: NetMessage) {
        let is_active = self.active_world.as_deref() == Some(msg.world.as_str());

        match msg.event {
            NetEvent::Line(bytes) => {
                let now = Instant::now();
                self.last_server_data = now;
                self.last_data_per_world.insert(msg.world.clone(), now);
                let text = String::from_utf8_lossy(&bytes).into_owned();

                // Trigger matching.
                let world_filter = Some(msg.world.as_str());
                let actions = self.macro_store.find_triggers(&text, world_filter);

                // Determine gag and merged attr from trigger actions.
                // Also honour the global %gag variable (/gag sets gag=1).
                let global_gag = self.interp.get_global_var("gag")
                    .map(|v| v.as_bool())
                    .unwrap_or(false);
                let gagged = global_gag || actions.iter().any(|a| a.gag);
                let merged_attr = actions.iter().fold(Attr::EMPTY, |acc, a| acc | a.attr);

                if !gagged {
                    let line = if merged_attr == Attr::EMPTY {
                        LogicalLine::plain(&text)
                    } else {
                        LogicalLine::new(
                            { let mut t = crate::tfstr::TfStr::new(); t.push_str(&text, None); t },
                            merged_attr,
                        )
                    };
                    self.screen.push_line(line);
                    // Write to log file if open.
                    if let Some(ref mut f) = self.log_file {
                        use tokio::io::AsyncWriteExt;
                        let _ = f.write_all(format!("{text}\n").as_bytes()).await;
                    }
                }

                // Execute trigger bodies.
                for ta in &actions {
                    if let Some(body) = &ta.body {
                        self.run_command(body).await;
                    }
                }

                // Fire ACTIVITY (active world) or BGTEXT (background world).
                let hook = if is_active { Hook::Activity } else { Hook::BgText };
                self.fire_hook(hook, &text).await;

                self.need_refresh = true;
            }
            NetEvent::Prompt(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let line = LogicalLine::plain(&text);
                self.screen.push_line(line);
                self.fire_hook(Hook::Prompt, &text).await;
                self.need_refresh = true;
            }
            NetEvent::Gmcp(module, payload) => {
                let arg = format!("{module} {payload}");
                self.fire_hook(Hook::Gmcp, &arg).await;
            }
            NetEvent::Atcp(func, val) => {
                let arg = format!("{func} {val}");
                self.fire_hook(Hook::Atcp, &arg).await;
            }
            NetEvent::Opt102(payload) => {
                self.fire_hook(Hook::Option102, &payload).await;
            }
            NetEvent::Closed => {
                let was_active = self.active_world.as_deref() == Some(&msg.world);
                self.handles.remove(&msg.world);
                self.world_order.retain(|w| *w != msg.world);
                if was_active {
                    self.active_world = self.world_order.first().cloned();
                }
                let notice = format!("** Connection to {} closed **", msg.world);
                self.screen.push_line(LogicalLine::plain(&notice));
                self.fire_hook(Hook::Disconnect, &msg.world).await;
                self.update_status();
                self.need_refresh = true;
            }
            NetEvent::McccpError(err) => {
                let notice = format!("** MCCP decompression error on {}: {err} — disconnecting **", msg.world);
                self.screen.push_line(LogicalLine::plain(&notice));
                // Drop the connection handle so the task is stopped.
                self.handles.remove(&msg.world);
                self.world_order.retain(|w| *w != msg.world);
                let was_active = self.active_world.as_deref() == Some(&msg.world);
                if was_active {
                    self.active_world = self.world_order.first().cloned();
                }
                self.fire_hook(Hook::Disconnect, &msg.world).await;
                self.update_status();
                self.need_refresh = true;
            }
        }
    }

    /// Fire all hooks of type `hook` with `args`, running matched macro bodies.
    ///
    /// Hook bodies are executed through the interpreter directly (without
    /// re-entering `run_command`) to avoid async recursion cycles.
    fn fire_hook_sync(&mut self, hook: Hook, args: &str) {
        let actions = self.macro_store.find_hooks(hook, args);
        for ta in actions {
            if let Some(body) = ta.body {
                if let Err(e) = self.interp.exec_script(&body) {
                    let msg = format!("% Hook error: {e}");
                    self.screen.push_line(LogicalLine::plain(&msg));
                    self.need_refresh = true;
                }
                for line in self.interp.output.drain(..) {
                    let content = crate::tfstr::TfStr::from_tf_markup(&line);
                    self.screen.push_line(LogicalLine::new(content, Attr::EMPTY));
                    self.need_refresh = true;
                }
                // Drain simple actions (AddWorld, DefMacro) but skip
                // Connect/Disconnect to avoid re-entrancy.
                for action in self.interp.take_actions() {
                    match action {
                        ScriptAction::AddWorld(w) => { self.worlds.upsert(w); }
                        ScriptAction::DefMacro(mac) => {
                            if let Some(n) = &mac.name { self.interp.macro_names.insert(n.clone()); }
                            self.macro_store.add(mac);
                        }
                        ScriptAction::Quit => { self.quit = true; }
                        _ => {} // Connect/Disconnect/Send deferred
                    }
                }
            }
        }
    }

    /// Async wrapper for fire_hook_sync (for call sites outside action handlers).
    pub async fn fire_hook(&mut self, hook: Hook, args: &str) {
        self.fire_hook_sync(hook, args);
    }

    // ── Process scheduler ─────────────────────────────────────────────────

    async fn run_due_processes(&mut self, now: Instant) {
        let ready = self.scheduler.take_ready(now);
        for mut proc in ready {
            let keep = self.execute_process(&mut proc).await;
            if keep && proc.tick() {
                self.scheduler.reschedule(proc);
            }
        }
    }

    /// Execute one scheduled process tick, returning `false` when it should be
    /// removed even if its run count hasn't expired (e.g. QuoteFile reached EOF).
    async fn execute_process(&mut self, proc: &mut crate::process::Proc) -> bool {
        use crate::process::ProcKind;
        match &mut proc.kind {
            ProcKind::Repeat { body } => {
                let body = body.clone();
                // Repeat bodies that look like TF commands are dispatched through
                // the interpreter; plain text is sent to the world.
                if body.starts_with('/') {
                    self.run_command(&body).await;
                } else {
                    let world = proc.world.clone().or_else(|| self.active_world.clone());
                    if let Some(w) = world {
                        if let Some(handle) = self.handles.get(&w) {
                            handle.send_line(&body).await;
                        }
                    }
                }
                true
            }

            ProcKind::QuoteFile { path, pos } => {
                use std::io::{BufRead, Seek, SeekFrom};
                let path = path.clone();
                let offset = *pos;
                // Read one line from the file at the current offset.
                let result: Option<(String, u64)> = (|| {
                    let mut file = std::fs::File::open(&path).ok()?;
                    file.seek(SeekFrom::Start(offset)).ok()?;
                    let mut reader = std::io::BufReader::new(file);
                    let mut line = String::new();
                    let n = reader.read_line(&mut line).ok()?;
                    if n == 0 {
                        return None; // EOF
                    }
                    let new_pos = offset + n as u64;
                    // Strip trailing \r\n.
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r').to_owned();
                    Some((trimmed, new_pos))
                })();
                if let Some((line, new_pos)) = result {
                    *pos = new_pos;
                    let world = proc.world.clone().or_else(|| self.active_world.clone());
                    if let Some(w) = world {
                        if let Some(handle) = self.handles.get(&w) {
                            handle.send_line(&line).await;
                        }
                    }
                    true
                } else {
                    false // EOF — stop the process
                }
            }

            ProcKind::QuoteShell { command } => {
                // Run the shell command once and send each line of its stdout
                // to the active world.  One-shot: always returns `false`.
                let command = command.clone();
                let world = proc.world.clone().or_else(|| self.active_world.clone());
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .output()
                    .await;
                if let Ok(out) = output {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for line in text.lines() {
                        if let Some(ref w) = world {
                            if let Some(handle) = self.handles.get(w) {
                                handle.send_line(line).await;
                            }
                        }
                    }
                }
                false
            }
        }
    }

    // ── Watchdog ──────────────────────────────────────────────────────────

    /// If watchdog is configured and the monitored world has been silent for
    /// longer than the watchdog interval, drop and reconnect it.
    async fn check_watchdog(&mut self, now: Instant) {
        let Some(interval) = self.watchdog_interval else { return };
        let world_name = self.watchdog_world.clone()
            .or_else(|| self.active_world.clone());
        let Some(world_name) = world_name else { return };

        // Only trigger for worlds that are currently connected.
        if !self.handles.contains_key(&world_name) {
            return;
        }

        let elapsed = self.last_data_per_world
            .get(&world_name)
            .map(|t| now.duration_since(*t))
            .unwrap_or(Duration::MAX);

        if elapsed >= interval {
            let msg = format!("** Watchdog: reconnecting to {world_name} **");
            self.screen.push_line(LogicalLine::plain(&msg));
            // Drop the stale handle; connection_task will notice its receiver closed.
            self.handles.remove(&world_name);
            self.world_order.retain(|w| *w != world_name);
            // Reset the timer so we don't trigger again immediately.
            self.last_data_per_world.insert(world_name.clone(), now);
            self.connect_world_by_name(&world_name).await;
            self.need_refresh = true;
        }
    }

    /// Earliest time the watchdog needs to fire, for the select! deadline.
    fn watchdog_deadline(&self) -> Option<Instant> {
        let interval = self.watchdog_interval?;
        let world = self.watchdog_world.as_deref()
            .or(self.active_world.as_deref())?;
        if !self.handles.contains_key(world) {
            return None;
        }
        let last = self.last_data_per_world.get(world)?;
        Some(*last + interval)
    }

    // ── Mail check ────────────────────────────────────────────────────────

    async fn check_mail(&mut self, now: Instant) {
        if now < self.mail_next {
            return;
        }
        self.mail_next = now + MAIL_CHECK_INTERVAL;
        if let Some(path) = self.mail_path.clone() {
            if std::fs::metadata(&path).is_ok() {
                self.fire_hook(Hook::Mail, path.display().to_string().as_str()).await;
            }
        }
    }

    // ── Display ───────────────────────────────────────────────────────────

    /// Rebuild the status bar by evaluating `status_format` tokens.
    ///
    /// Supported tokens: `%world` (active world name), `%T` (HH:MM),
    /// `%t` (HH:MM:SS).
    fn update_status(&mut self) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let world = self.active_world.as_deref().unwrap_or("").to_owned();
        // Keep interpreter globals in sync so worldname() / nworlds() etc. are accurate.
        self.interp.set_global_var("worldname", crate::script::Value::Str(world.clone()));
        self.interp.set_global_var("nworlds",   crate::script::Value::Int(self.handles.len() as i64));
        // fg_world() is an alias for the active world name.
        self.interp.set_global_var("fg_world",  crate::script::Value::Str(world.clone()));
        // Space-separated list of all open world names (for is_open / is_connected).
        let open: String = self.handles.keys().cloned().collect::<Vec<_>>().join(" ");
        self.interp.set_global_var("_open_worlds", crate::script::Value::Str(open));
        // nactive: all open connections count as active.
        self.interp.set_global_var("nactive", crate::script::Value::Int(self.handles.len() as i64));
        // nlog: 1 when a log file is open, 0 otherwise.
        let nlog = self.log_file.is_some() as i64;
        self.interp.set_global_var("nlog", crate::script::Value::Int(nlog));
        // Snapshot all world definitions for world_info() lookups.
        self.interp.worlds_snapshot = self.worlds.iter()
            .map(|w| (w.name.clone(), w.clone()))
            .collect();
        // Build the status line: use named fields if populated, else simple format.
        let text = if self.status_fields.is_empty() {
            let world = if world.is_empty() { "(no world)".to_owned() } else { world };
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let hh = (secs % 86400) / 3600;
            let mm = (secs % 3600) / 60;
            let ss = secs % 60;
            self.status_format
                .replace("%world", &world)
                .replace("%T", &format!("{hh:02}:{mm:02}"))
                .replace("%t", &format!("{hh:02}:{mm:02}:{ss:02}"))
        } else {
            self.build_status_text()
        };
        self.status = StatusLine::new(text);
    }

    /// Rebuild the `%status_fields` global from the current field list.
    fn sync_status_fields_global(&mut self) {
        let spec = self.status_fields.iter()
            .map(|f| f.to_spec())
            .collect::<Vec<_>>()
            .join(" ");
        self.interp.set_global_var("status_fields", crate::script::Value::Str(spec));
    }

    /// Evaluate each status field expression and concatenate into one string.
    ///
    /// Dynamic fields (`@`) evaluate `status_int_<name>` or `status_var_<name>`
    /// as a TF expression.  Static fields read `status_var_<name>` as a variable
    /// name and return its current value.  Empty-name fields are spacers.
    fn build_status_text(&mut self) -> String {
        use crate::script::expr::eval_str;
        let width = self.terminal.width as usize;
        let fields = self.status_fields.clone();

        // Pass 1: evaluate every field's content.
        let mut contents: Vec<String> = Vec::with_capacity(fields.len());
        for field in &fields {
            let content = if field.name.is_empty() {
                // Spacer field: always has an explicit width; fill is handled in pass 2.
                String::new()
            } else {
                let int_key = format!("status_int_{}", field.name);
                let var_key = format!("status_var_{}", field.name);
                let expr = self.interp.get_global_var(&int_key)
                    .or_else(|| self.interp.get_global_var(&var_key))
                    .map(|v: &crate::script::Value| v.to_string())
                    .unwrap_or_default();
                if expr.is_empty() {
                    String::new()
                } else {
                    eval_str(&expr, &mut self.interp)
                        .unwrap_or_default()
                        .to_string()
                }
            };
            contents.push(content);
        }

        // Pass 2: compute how much space each auto-width field (width=None) gets.
        // Fixed-width fields (including spacers) consume a known number of columns;
        // the remainder is divided equally among auto-width fields.  This makes
        // fields like @world elastic so the clock ends up at the right edge.
        let fixed_total: usize = fields.iter().filter_map(|f| f.width).sum();
        let auto_count = fields.iter().filter(|f| f.width.is_none()).count();
        let auto_width = if auto_count > 0 {
            width.saturating_sub(fixed_total) / auto_count
        } else {
            0
        };

        // Pass 3: build the final string with proper padding/truncation.
        // C TF uses '_' as the fill character for the status bar.
        let pad_to = |s: &str, target: usize| -> String {
            let chars: Vec<char> = s.chars().collect();
            if chars.len() >= target {
                chars[..target].iter().collect()
            } else {
                let mut out: String = chars.iter().collect();
                while out.chars().count() < target { out.push('_'); }
                out
            }
        };

        let mut text = String::new();
        for (field, content) in fields.iter().zip(contents.iter()) {
            if text.chars().count() >= width {
                break;
            }
            let target = match field.width {
                Some(w) => w,
                None => auto_width,
            };
            // Spacer fields render as underscores; named fields use their content.
            let src = if field.name.is_empty() { "" } else { content.as_str() };
            text.push_str(&pad_to(src, target));
        }
        text
    }

    /// Sync keyboard-state interpreter globals: `kbpoint`, `kbhead`, `kbtail`.
    fn sync_kb_globals(&mut self) {
        use crate::script::Value;
        let pos = self.input.editor.pos;
        // Build head/tail directly from the char buffer — avoids allocating
        // a full String representation just to re-split it.
        let chars = self.input.editor.chars();
        let head: String = chars[..pos].iter().collect();
        let tail: String = chars[pos..].iter().collect();
        self.interp.set_global_var("kbpoint", Value::Int(pos as i64));
        self.interp.set_global_var("kbhead",  Value::Str(head));
        self.interp.set_global_var("kbtail",  Value::Str(tail));
        self.interp.set_global_var("insert",  Value::Int(self.input.editor.insert_mode as i64));
        // kbnum: numeric prefix (M-<digit> prefix count); not yet implemented, always 0.
        self.interp.set_global_var("kbnum", Value::Int(0));
    }

    /// Render the screen, status bar, and input line, then flush.
    fn refresh_display(&mut self) {
        // sync_kb_globals must run before update_status so that the `insert`,
        // `kbpoint`, etc. globals are current when build_status_text evaluates
        // status field expressions (e.g. `insert ? "" : "(Over)"`).
        self.sync_kb_globals();
        self.update_status();
        let scrollback = self.screen.scrollback();
        self.interp.set_global_var("moresize", crate::script::Value::Int(scrollback as i64));
        self.interp.set_global_var("columns",  crate::script::Value::Int(self.terminal.width as i64));
        self.interp.set_global_var("winlines", crate::script::Value::Int(self.terminal.height as i64));
        let idle_secs  = self.last_keystroke.elapsed().as_secs_f64();
        let sidle_secs = self.last_server_data.elapsed().as_secs_f64();
        self.interp.set_global_var("_idle",  crate::script::Value::Float(idle_secs));
        self.interp.set_global_var("_sidle", crate::script::Value::Float(sidle_secs));
        let morepaused = self.screen.paused as i64;
        self.interp.set_global_var("_morepaused", crate::script::Value::Int(morepaused));
        let status = self.status.clone();
        let _ = self.terminal.render_screen(&self.screen);
        let _ = self.terminal.render_status(std::slice::from_ref(&status));
        if self.screen.paused {
            let _ = self.terminal.show_more_prompt();
        }
        let prompt_len = self.input_prompt.chars().count();
        let text = format!("{}{}", self.input_prompt, self.input.editor.text());
        let pos  = self.input.editor.pos + prompt_len;
        let _ = self.terminal.render_input(&text, pos);
        let _ = self.terminal.flush();
    }

    // ── Shutdown ──────────────────────────────────────────────────────────

    fn shutdown(&mut self) {
        self.handles.clear();
        self.world_order.clear();
        self.scheduler.kill_all();
        let _ = self.terminal.flush();
    }
}

impl Default for EventLoop {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybind::{DoKeyOp, KeyBinding};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn key_decoder_printable_ascii() {
        let mut kd = KeyDecoder::new();
        assert_eq!(kd.push(b'a'), Some(EditAction::InsertChar('a')));
        assert_eq!(kd.push(b'Z'), Some(EditAction::InsertChar('Z')));
    }

    #[test]
    fn key_decoder_ctrl_m_is_newline() {
        let mut kd = KeyDecoder::new();
        let action = kd.push(b'\r'); // Ctrl-M = 0x0D
        assert_eq!(
            action,
            Some(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::Newline)))
        );
    }

    #[test]
    fn key_decoder_ctrl_j_is_newline() {
        let mut kd = KeyDecoder::new();
        assert_eq!(
            kd.push(b'\n'),
            Some(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::Newline)))
        );
    }

    #[test]
    fn key_decoder_up_arrow_three_bytes() {
        let mut kd = KeyDecoder::new();
        assert_eq!(kd.push(0x1B), None); // ESC — wait
        assert_eq!(kd.push(b'['), None); // ESC [ — wait
        // Arrow up = ESC [ A → RecallBackward
        assert_eq!(
            kd.push(b'A'),
            Some(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::RecallBackward)))
        );
    }

    #[test]
    fn key_decoder_down_arrow() {
        let mut kd = KeyDecoder::new();
        kd.push(0x1B);
        kd.push(b'[');
        assert_eq!(
            kd.push(b'B'),
            Some(EditAction::Bound(KeyBinding::DoKey(DoKeyOp::RecallForward)))
        );
    }

    #[test]
    fn event_loop_constructs() {
        let el = EventLoop::new();
        assert!(el.active_world.is_none());
        assert!(el.handles.is_empty());
    }

    #[tokio::test]
    async fn connect_registers_active_world() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { listener.accept().await });

        let mut el = EventLoop::new();
        el.connect("mud", "127.0.0.1", addr.port()).await.unwrap();
        assert_eq!(el.active_world.as_deref(), Some("mud"));
        assert!(el.handles.contains_key("mud"));
    }

    #[tokio::test]
    async fn send_line_reaches_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64];
            let n = sock.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let mut el = EventLoop::new();
        el.connect("test", "127.0.0.1", addr.port()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        el.send_to_active("hello world").await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let received = server.await.unwrap();
        assert_eq!(&received, b"hello world\r\n");
    }

    #[tokio::test]
    async fn net_events_arrive_via_channel() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(b"first line\r\nsecond line\r\n").await.unwrap();
        });

        let mut el = EventLoop::new();
        el.connect("mud", "127.0.0.1", addr.port()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.await.unwrap();

        let mut lines: Vec<String> = Vec::new();
        while let Ok(msg) = el.net_rx.try_recv() {
            if let NetEvent::Line(b) = msg.event {
                lines.push(String::from_utf8(b).unwrap());
            }
        }
        assert_eq!(lines, vec!["first line", "second line"]);
    }

    #[tokio::test]
    async fn closed_event_removes_handle() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (_sock, _) = listener.accept().await.unwrap();
            // Drop _sock immediately → EOF for client
        });

        let mut el = EventLoop::new();
        el.connect("gone", "127.0.0.1", addr.port()).await.unwrap();
        server.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        while let Ok(msg) = el.net_rx.try_recv() {
            el.handle_net_message(msg).await;
        }
        assert!(!el.handles.contains_key("gone"));
    }

    // ── S3: -l / -q flag tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn login_hook_fires_with_credentials() {
        // World with character+pass → LOGIN hook should fire after connect.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { listener.accept().await });

        let mut el = EventLoop::new();
        // Register a macro that records when the LOGIN hook fires.
        el.interp.exec_script(
            "/def -hLOGIN log_login = /set _login_fired 1"
        ).unwrap();
        for action in el.interp.take_actions() {
            el.handle_script_action(action).await;
        }

        // Add a world with credentials and connect.
        let mut w = crate::world::World::named("testworld");
        w.host = Some("127.0.0.1".into());
        w.port = Some(addr.port().to_string());
        w.character = Some("hero".into());
        w.pass = Some("secret".into());
        el.worlds.upsert(w);
        el.connect_world_by_name("testworld").await;

        // Hook should have set _login_fired.
        assert_eq!(
            el.interp.get_global_var("_login_fired"),
            Some(&crate::script::value::Value::Int(1)),
            "LOGIN hook should fire when autologin is enabled and world has credentials"
        );
    }

    #[tokio::test]
    async fn login_hook_suppressed_with_no_autologin() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { listener.accept().await });

        let mut el = EventLoop::new();
        el.no_autologin = true; // -l flag
        el.interp.exec_script(
            "/def -hLOGIN log_login = /set _login_fired 1"
        ).unwrap();
        for action in el.interp.take_actions() {
            el.handle_script_action(action).await;
        }

        let mut w = crate::world::World::named("testworld");
        w.host = Some("127.0.0.1".into());
        w.port = Some(addr.port().to_string());
        w.character = Some("hero".into());
        w.pass = Some("secret".into());
        el.worlds.upsert(w);
        el.connect_world_by_name("testworld").await;

        assert_eq!(
            el.interp.get_global_var("_login_fired"),
            None,
            "LOGIN hook must NOT fire when -l (no_autologin) is set"
        );
    }
}
