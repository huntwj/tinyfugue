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

use crate::keybind::{EditAction, InputProcessor, Keymap};
use crate::net::{Connection, NetEvent};
use crate::process::ProcessScheduler;
use crate::script::interp::{FileLoader, Interpreter, ScriptAction};
use crate::terminal::Terminal;
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

    /// Set to `true` to exit the main loop after the current iteration.
    quit: bool,

    /// Path to check for new mail (mirrors `%mailpath`).
    mail_path: Option<PathBuf>,
    mail_next: Instant,

    /// True when the screen needs a full redraw.
    need_refresh: bool,
}

impl EventLoop {
    /// Create a new, idle event loop (no connections open yet).
    pub fn new() -> Self {
        let (net_tx, net_rx) = mpsc::channel(256);
        let terminal = Terminal::new(std::io::stdout())
            .expect("failed to create terminal");
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
            quit: false,
            mail_path: None,
            mail_next: Instant::now() + MAIL_CHECK_INTERVAL,
            need_refresh: false,
        }
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
            if let ScriptAction::AddWorld(w) = action {
                self.worlds.upsert(w);
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
                            for &b in &stdin_buf[..n] {
                                if let Some(action) = self.key_decoder.push(b) {
                                    if let Some(line) = self.input.apply(action) {
                                        self.dispatch_line(line).await;
                                    }
                                }
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
                        self.need_refresh = true;
                    }
                }

                // Graceful shutdown.
                _ = sigterm.recv() => self.quit = true,
                _ = sigint.recv()  => self.quit = true,
                _ = sighup.recv()  => self.quit = true,

                // Timer tick.
                _ = &mut timer => {
                    let now = Instant::now();
                    self.run_due_processes(now).await;
                    self.check_mail(now);
                    if self.need_refresh {
                        let _ = self.terminal.flush();
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
            self.send_to_active(&line).await;
        }
    }

    async fn run_command(&mut self, cmd: &str) {
        if let Err(e) = self.interp.exec_script(cmd) {
            self.terminal.print_line(&format!("% Error: {e}"));
            self.need_refresh = true;
        }
        // Print interpreter output to terminal.
        for line in self.interp.output.drain(..) {
            self.terminal.print_line(&line);
            self.need_refresh = true;
        }
        // Process queued side-effects.
        for action in self.interp.take_actions() {
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
                    self.terminal
                        .print_line(&format!("** Disconnected from {n} **"));
                    self.need_refresh = true;
                }
            }

            ScriptAction::AddWorld(w) => {
                self.worlds.upsert(w);
            }

            ScriptAction::SwitchWorld { name } => {
                if self.handles.contains_key(&name) {
                    self.active_world = Some(name);
                } else {
                    self.terminal
                        .print_line(&format!("% No open connection to '{name}'"));
                    self.need_refresh = true;
                }
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
            self.terminal
                .print_line(&format!("% Unknown world '{name}'"));
            self.need_refresh = true;
            return;
        };
        if !w.is_connectable() {
            self.terminal
                .print_line(&format!("% World '{}' has no host/port", w.name));
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
        if let Err(e) = result {
            self.terminal
                .print_line(&format!("% Connect to '{}' failed: {e}", w.name));
            self.need_refresh = true;
        }
    }

    // ── Net event dispatch ────────────────────────────────────────────────

    pub(crate) async fn handle_net_message(&mut self, msg: NetMessage) {
        match msg.event {
            NetEvent::Line(bytes) => {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    self.terminal.print_line(text);
                    self.need_refresh = true;
                }
            }
            NetEvent::Prompt(bytes) => {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    self.terminal.print_line(text);
                    self.need_refresh = true;
                }
            }
            NetEvent::Gmcp(module, payload) => {
                // Fire GMCP hooks — wired to MacroStore in Phase 10.
                let _ = (module, payload);
            }
            NetEvent::Atcp(func, val) => {
                let _ = (func, val);
            }
            NetEvent::Closed => {
                let was_active = self.active_world.as_deref() == Some(&msg.world);
                self.handles.remove(&msg.world);
                if was_active {
                    self.active_world = self.handles.keys().next().cloned();
                }
                self.terminal.print_line(
                    &format!("** Connection to {} closed **", msg.world),
                );
                self.need_refresh = true;
            }
        }
    }

    // ── Process scheduler ─────────────────────────────────────────────────

    async fn run_due_processes(&mut self, now: Instant) {
        let ready = self.scheduler.take_ready(now);
        for mut proc in ready {
            self.execute_process(&proc).await;
            if proc.tick() {
                self.scheduler.reschedule(proc);
            }
        }
    }

    async fn execute_process(&mut self, proc: &crate::process::Proc) {
        use crate::process::ProcKind;
        match &proc.kind {
            ProcKind::Repeat { body } => {
                let world = proc.world.clone().or_else(|| self.active_world.clone());
                if let Some(w) = world {
                    if let Some(handle) = self.handles.get(&w) {
                        handle.send_line(body).await;
                    }
                }
            }
            ProcKind::QuoteFile { .. } | ProcKind::QuoteShell { .. } => {
                // Full implementation in Phase 10 with tokio::process.
            }
        }
    }

    // ── Mail check ────────────────────────────────────────────────────────

    fn check_mail(&mut self, now: Instant) {
        if now < self.mail_next {
            return;
        }
        self.mail_next = now + MAIL_CHECK_INTERVAL;
        if let Some(path) = &self.mail_path {
            if std::fs::metadata(path).is_ok() {
                // Full implementation fires the MAIL hook via MacroStore.
            }
        }
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
