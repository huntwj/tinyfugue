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

use tokio::io::AsyncReadExt;
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
    loop {
        tokio::select! {
            result = conn.recv() => {
                match result {
                    Ok(events) => {
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
        std::fs::read_to_string(&resolved)
            .map_err(|e| format!("{resolved}: {e}"))
    })
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
    log_file: Option<std::fs::File>,

    /// Format string for the status bar.  Tokens: `%world`, `%T` (HH:MM),
    /// `%t` (HH:MM:SS).  Defaults to `"[ %world ]  %T"`.
    status_format: String,

    /// When the user last pressed a key (for `idle()`).
    last_keystroke: Instant,
    /// When data was last received from any server (for `sidle()`).
    last_server_data: Instant,
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
        }
    }

    /// Returns `true` if there is a default world to auto-connect to.
    pub fn has_default_world(&self) -> bool {
        self.worlds.default_world().is_some()
    }

    /// Execute a `.tf` script file through the interpreter.
    ///
    /// Output lines are printed immediately; [`ScriptAction`]s that make sense
    /// during startup (e.g. `AddWorld`) are processed; others are dropped.
    /// Returns `Err` if the file cannot be read.
    pub fn load_script_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.interp
            .exec_script(&src)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // Drain output — print to stdout (terminal not in raw mode yet).
        for line in self.interp.output.drain(..) {
            println!("{line}");
        }
        // Process startup-safe actions.
        for action in self.interp.take_actions() {
            // Quit/Connect/Disconnect are deferred until after startup.
            match action {
                ScriptAction::AddWorld(w) => { self.worlds.upsert(w); }
                ScriptAction::DefMacro(mac) => { self.macro_store.add(mac); }
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

        let mut stdin = tokio::io::stdin();
        let mut stdin_buf = [0u8; 256];

        while !self.quit {
            // ── Compute the next timer deadline ──────────────────────────
            let now = Instant::now();
            let deadline = [
                self.scheduler.next_wakeup(),
                Some(self.mail_next),
                Some(now + REFRESH_INTERVAL),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + Duration::from_secs(3600));

            let timer = sleep_until(deadline.into());
            tokio::pin!(timer);

            // ── Select ───────────────────────────────────────────────────
            tokio::select! {
                // Keyboard input.
                result = stdin.read(&mut stdin_buf) => {
                    match result {
                        Ok(0) | Err(_) => self.quit = true,
                        Ok(n) => {
                            self.last_keystroke = Instant::now();
                            for &b in &stdin_buf[..n] {
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
                                let text = self.input.editor.text();
                                let pos  = self.input.editor.pos;
                                let _ = self.terminal.render_input(&text, pos);
                                let _ = self.terminal.flush();
                            }
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
                    if self.need_refresh {
                        self.refresh_display();
                        self.need_refresh = false;
                    }
                }
            }
        }

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

            ScriptAction::SendToWorld { text, world } => {
                let name = world.or_else(|| self.active_world.clone());
                if let Some(n) = name {
                    if let Some(h) = self.handles.get(&n) {
                        h.send_line(&text).await;
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
                    if self.active_world.as_deref() == Some(&n) {
                        self.active_world = self.handles.keys().next().cloned();
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
                let _ = self.macro_store.add(mac);
            }

            ScriptAction::SwitchWorld { name } => {
                if self.handles.contains_key(&name) {
                    self.active_world = Some(name);
                    self.update_status();
                } else {
                    let msg = format!("% No open connection to '{name}'");
                    self.screen.push_line(LogicalLine::plain(&msg));
                    self.need_refresh = true;
                }
            }

            // ── Process scheduling ─────────────────────────────────────────

            ScriptAction::AddRepeat { interval_ms, count, body, world } => {
                let interval = Duration::from_millis(interval_ms);
                self.scheduler.add_repeat(body, interval, count, world);
            }

            ScriptAction::AddQuoteFile { interval_ms, path, world } => {
                let interval = Duration::from_millis(interval_ms);
                self.scheduler.add_quote_file(PathBuf::from(path), interval, -1, world);
            }

            ScriptAction::AddQuoteShell { interval_ms, command, world } => {
                let interval = Duration::from_millis(interval_ms);
                self.scheduler.add_quote_shell(command, interval, -1, world);
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
                match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
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
                use std::io::Write;
                let lines: Vec<String> = self
                    .worlds
                    .iter()
                    .filter(|w| name.as_deref().is_none_or(|n| w.name == n))
                    .map(|w| w.to_addworld())
                    .collect();
                match path {
                    Some(p) => {
                        match std::fs::OpenOptions::new()
                            .create(true).write(true).truncate(true).open(&p)
                        {
                            Ok(mut f) => {
                                for line in &lines { let _ = writeln!(f, "{line}"); }
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
                let _ = std::process::Command::new(&shell).status();
                crossterm::terminal::enable_raw_mode().ok();
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
                        use std::io::Write;
                        match std::fs::File::create(&p) {
                            Ok(mut f) => {
                                for line in &lines {
                                    let _ = writeln!(f, "{line}");
                                }
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
                        let runs = if p.runs_left == -1 {
                            "∞".to_owned()
                        } else {
                            p.runs_left.to_string()
                        };
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
        }
    }

    /// Connect to a world by name (looks it up in `WorldStore`).
    /// An empty name means the default world.
    pub async fn connect_world_by_name(&mut self, name: &str) {
        let world = if name.is_empty() {
            self.worlds.default_world().cloned()
        } else {
            self.worlds.find(name).cloned()
        };
        let Some(w) = world else {
            let msg = format!("% Unknown world '{name}'");
            self.screen.push_line(LogicalLine::plain(&msg));
            self.need_refresh = true;
            return;
        };
        if !w.is_connectable() {
            let msg = format!("% World '{}' has no host/port", w.name);
            self.screen.push_line(LogicalLine::plain(&msg));
            self.need_refresh = true;
            return;
        }
        let host = w.host.as_deref().unwrap();
        let port: u16 = w.port.as_deref().unwrap_or("23").parse().unwrap_or(23);
        let result = if w.flags.ssl {
            self.connect_tls(&w.name, host, port).await
        } else {
            self.connect(&w.name, host, port).await
        };
        match result {
            Ok(()) => {
                let world_name = w.name.clone();
                self.update_status();
                self.fire_hook(Hook::Connect, &world_name).await;
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
                self.last_server_data = Instant::now();
                let text = String::from_utf8_lossy(&bytes).into_owned();

                // Trigger matching.
                let world_filter = Some(msg.world.as_str());
                let actions = self.macro_store.find_triggers(&text, world_filter);

                // Determine gag and merged attr from trigger actions.
                let gagged = actions.iter().any(|a| a.gag);
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
                        use std::io::Write;
                        let _ = writeln!(f, "{text}");
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
            NetEvent::Closed => {
                let was_active = self.active_world.as_deref() == Some(&msg.world);
                self.handles.remove(&msg.world);
                if was_active {
                    self.active_world = self.handles.keys().next().cloned();
                }
                let notice = format!("** Connection to {} closed **", msg.world);
                self.screen.push_line(LogicalLine::plain(&notice));
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
                        ScriptAction::DefMacro(mac) => { self.macro_store.add(mac); }
                        ScriptAction::Quit => { self.quit = true; }
                        _ => {} // Connect/Disconnect/Send deferred
                    }
                }
            }
        }
    }

    /// Async wrapper for fire_hook_sync (for call sites outside action handlers).
    async fn fire_hook(&mut self, hook: Hook, args: &str) {
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
        let world = if world.is_empty() { "(no world)".to_owned() } else { world };
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let hh = (secs % 86400) / 3600;
        let mm = (secs % 3600) / 60;
        let ss = secs % 60;
        let text = self.status_format
            .replace("%world", &world)
            .replace("%T", &format!("{hh:02}:{mm:02}"))
            .replace("%t", &format!("{hh:02}:{mm:02}:{ss:02}"));
        self.status = StatusLine::new(text);
    }

    /// Sync keyboard-state interpreter globals: `kbpoint`, `kbhead`, `kbtail`.
    fn sync_kb_globals(&mut self) {
        use crate::script::Value;
        let pos = self.input.editor.pos;
        let text = self.input.editor.text();
        let (head, tail): (String, String) = {
            let chars: Vec<char> = text.chars().collect();
            (chars[..pos].iter().collect(), chars[pos..].iter().collect())
        };
        self.interp.set_global_var("kbpoint", Value::Int(pos as i64));
        self.interp.set_global_var("kbhead",  Value::Str(head));
        self.interp.set_global_var("kbtail",  Value::Str(tail));
    }

    /// Render the screen, status bar, and input line, then flush.
    fn refresh_display(&mut self) {
        self.update_status();
        self.sync_kb_globals();
        let scrollback = self.screen.scrollback();
        self.interp.set_global_var("moresize", crate::script::Value::Int(scrollback as i64));
        self.interp.set_global_var("columns",  crate::script::Value::Int(self.terminal.width as i64));
        self.interp.set_global_var("winlines", crate::script::Value::Int(self.terminal.height as i64));
        let idle_secs  = self.last_keystroke.elapsed().as_secs_f64();
        let sidle_secs = self.last_server_data.elapsed().as_secs_f64();
        self.interp.set_global_var("_idle",  crate::script::Value::Float(idle_secs));
        self.interp.set_global_var("_sidle", crate::script::Value::Float(sidle_secs));
        let status = self.status.clone();
        let _ = self.terminal.render_screen(&self.screen);
        let _ = self.terminal.render_status(std::slice::from_ref(&status));
        if self.screen.paused {
            let _ = self.terminal.show_more_prompt();
        }
        let text = self.input.editor.text();
        let pos  = self.input.editor.pos;
        let _ = self.terminal.render_input(&text, pos);
        let _ = self.terminal.flush();
    }

    // ── Shutdown ──────────────────────────────────────────────────────────

    fn shutdown(&mut self) {
        self.handles.clear();
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
    use tokio::io::AsyncWriteExt;
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
}
