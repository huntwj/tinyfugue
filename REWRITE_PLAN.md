# TinyFugue → Rust Rewrite Plan

## Philosophy

This is a gradual, *strangler-fig* rewrite: the C binary continues to work throughout. A parallel Rust binary grows phase by phase until it reaches feature parity, at which point the C code is retired. No big-bang cutover.

**Port order follows the dependency graph**: leaf modules (no internal deps) first, core event loop last.

**Status: all 15 phases complete. The Rust binary is the primary binary. 409 tests pass, zero clippy warnings.**

---

## Phase 0: Scaffolding ✓

**Goal**: Rust workspace exists, builds, and is integrated into the project tooling.

- [x] Create `tf-rs/` Cargo workspace with a `tf` lib crate and `tf` binary
- [x] Add `just build` and `just run` tasks
- [x] Add Rust build to CI (`build.yml`)

---

## Phase 1: Core Data Types ✓

**C source**: `dstring.c`, `attr.c`, `search.c`, `malloc.c`

**Goal**: Establish the foundational types that all other modules depend on.

- [x] `TfStr` — owned string with optional per-character `Attr` vector (`tf-rs/src/tfstr.rs`)
- [x] `Attr` — text attribute flags (bold, underline, fg/bg color) as a `bitflags!` type (`tf-rs/src/attr.rs`)
- [x] No external crate dependencies in this phase

---

## Phase 2: Pattern Matching ✓

**C source**: `pattern.c` (wraps PCRE2)

**Goal**: Encapsulate regex behind a `Pattern` enum so the backing engine can be swapped.

- [x] `Pattern` enum: `Regexp` (via `regex` crate), `Glob`, `Simple`, `Substr`
- [x] Named capture groups, case-insensitive matching, substring extraction
- [x] `MatchMode` enum for `/def -m` flag (`tf-rs/src/pattern.rs`)

**Crates**: `regex`, `aho-corasick`

---

## Phase 3: World & Configuration Model ✓

**C source**: `world.c`, `variable.c`

**Goal**: Data model for MUD connections and user-configurable variables.

- [x] `World` struct: host, port, character, SSL flag, type (`tf-rs/src/world.rs`)
- [x] `WorldStore` with lookup and iteration
- [x] Global variable store (HashMap-backed, per-interpreter globals)

---

## Phase 4: TF Scripting Language ✓

**C source**: `expand.c`, `expr.c`, `parse.h`, `opcodes.h`, `command.c`, `cmdlist.h`

**Goal**: A working TF script interpreter.

- [x] Lexer + recursive descent parser producing an AST (`tf-rs/src/script/parser.rs`)
- [x] `Stmt` and `Expr` AST nodes for all TF constructs
- [x] Stack-based expression evaluator with `Value` enum (`Int`, `Float`, `Str`)
- [x] Built-in commands: `/send`, `/set`, `/let`, `/if`, `/while`, `/for`, `/return`, `/echo`, etc.
- [x] Variable substitution: `%var`, `%{var}`, `${var}`, positional args `{1}`…`{#}`, `{*}`, `{L}`, `{-L}`, `{-N}`, `{name-default}`, `{N-default}` (`tf-rs/src/script/expand.rs`)
- [x] Built-in functions: string, math, time, type inspection (`tf-rs/src/script/builtins.rs`)

---

## Phase 5: Macro & Trigger System ✓

**C source**: `macro.c`, `hooklist.h`, `enumlist.h`

**Goal**: The trigger/hook engine that connects server output to TF scripts.

- [x] `Macro` struct: name, body, trigger pattern, key binding, priority, world scope, flags (`tf-rs/src/macro_store.rs`)
- [x] `Hook` enum: CONNECT, DISCONNECT, ACTIVITY, SEND, PROMPT, MAIL, SIGHUP, SIGTERM
- [x] `MacroStore`: priority-ordered trigger matching, hook sets, key binding lookup
- [x] `/def`, `/trigger`, `/hook`, `/bind` all produce `Macro` entries via `parse_def()`

---

## Phase 6: Terminal Output ✓

**C source**: `output.c`, `tty.c`

**Goal**: Render MUD output and the status line to the terminal.

- [x] `Screen`: logical lines, physical (wrapped) lines, scrollback buffer (`tf-rs/src/screen.rs`)
- [x] ANSI attribute rendering from `Attr` values via `crossterm`
- [x] Status line with world name and clock (`tf-rs/src/terminal.rs`)
- [x] `crossterm` for cross-platform terminal control

**Crates**: `crossterm`

---

## Phase 7: Keyboard & Input Handling ✓

**C source**: `keyboard.c`, `keylist.h`

**Goal**: Read and edit user input, dispatch key bindings.

- [x] `LineEditor`: cursor movement, kill/yank ring, input history recall (`tf-rs/src/input.rs`)
- [x] `Keymap` and key binding lookup against macro table
- [x] `InputProcessor` driving `DoKeyOp` dispatch
- [x] `KeyDecoder`: multi-byte escape sequence parsing

---

## Phase 8: Networking & Telnet ✓

**C source**: `socket.c` (~4,000 lines), `tfselect.h`

**Goal**: Async multi-connection TCP client with Telnet protocol support.

- [x] `tokio`-based async I/O (one task per connection) (`tf-rs/src/net/`)
- [x] Telnet option negotiation FSM: NAWS, CHARSET, TTYPE, ECHO
- [x] MCCP decompression via `flate2`
- [x] TLS via `tokio-rustls` + `webpki-roots` (replaces OpenSSL)

**Crates**: `tokio`, `tokio-rustls`, `webpki-roots`, `flate2`

---

## Phase 9: Main Event Loop & Signal Handling ✓

**C source**: `main.c`, `signals.c`, `process.c`, `timers.c`

**Goal**: The top-level runtime tying all subsystems together.

- [x] `tokio::select!`-based loop over: keyboard input, socket activity, timers, signals (`tf-rs/src/event_loop.rs`)
- [x] Per-connection mpsc channels; `connection_task` per world
- [x] `ProcessScheduler` for `/repeat` and `/quote` processes
- [x] SIGWINCH (terminal resize), SIGTERM, SIGINT handling

---

## Phase 10: Optional Embedded Scripting ✓

**C source**: `lua.c`, `tfpython.c`

**Goal**: Restore optional Lua and Python embedding in the Rust binary.

- [x] Lua via `mlua` 0.10 (`tf-rs/src/lua_engine.rs`), behind `--features lua`
- [x] Python via `pyo3` 0.22 (`tf-rs/src/python_engine.rs`), behind `--features python`
- [x] `/loadlua`, `/calllua`, `/purgelua` dispatch (ScriptAction wired to event loop)
- [x] `/python`, `/callpython`, `/loadpython`, `/killpython` dispatch

**Crates**: `mlua` (optional), `pyo3` (optional)

---

## Phase 11: Script Parser Fixes ✓

**C source**: `parse.h`, `command.c`

**Goal**: The TF script parser correctly handles every file in `lib/tf/`.

- [x] `/for var start end body` range syntax (C-style for was incorrect)
- [x] EOF closes open `/if`…`/endif` and `/while`…`/done` blocks implicitly
- [x] `elseif` / inline-if forms
- [x] `!~` and `!/` (negated match operators)
- [x] All 47 `lib/tf/*.tf` files parse without error

---

## Phase 12: Startup, Configuration & Command Dispatch ✓

**C source**: `main.c`, `command.c`, `cmdlist.h`, `variable.c`

**Goal**: The binary loads user configuration on startup and routes every typed command through the script VM.

- [x] CLI argument parsing: `-f`, `-L`, `-c`, `-n`, `-l`, `-q`, `-v`, `-d`, world/host+port positionals (`tf-rs/src/cli.rs`)
- [x] Multiple `-c` flags accumulate with `%;` separator
- [x] Load `$TFLIBDIR/stdlib.tf` on startup (hard error if missing)
- [x] Load user config: `-f file`, or search `~/.tfrc`, `~/tfrc`, `./.tfrc`, `./tfrc`
- [x] All `/commands` dispatched through `Interpreter::exec_builtin`
- [x] `ScriptAction` enum carries deferred actions from interpreter to event loop

---

## Phase 13: Display, Triggers & Hooks ✓

**C source**: `output.c`, `tty.c`, `macro.c`, `hooklist.h`

**Goal**: Server output flows through the trigger/hook engine and the full Screen model.

- [x] `Screen::push_line()` used for all output; scrollback, wrapping work end-to-end
- [x] Status line rendered from format string (world name, clock)
- [x] Incoming lines run through `MacroStore` trigger matching before display
- [x] Hook dispatch: CONNECT, DISCONNECT, ACTIVITY, SEND, PROMPT, MAIL, SIGHUP, SIGTERM
- [x] Scrollback navigation bound to PgUp / PgDn via `DoKeyOp::ScrollUp/Down`
- [x] `parse_def()` builds full `Macro` from all flag combinations

---

## Phase 14: Processes & Logging ✓

**C source**: `process.c`, `lua.c`, `tfpython.c`

**Goal**: Process scheduling and introspection commands fully wired.

- [x] `/repeat interval count body` and `/quote 'file` / `/quote !cmd` through `ProcessScheduler`
- [x] `/log path`, `/nolog` session logging
- [x] `/listworlds`, `/list [filter]` introspection
- [x] `/undef`, `/unbind` macro/binding removal

---

## Phase 15: Cutover ✓

- [x] Feature parity verified: 409 unit tests + 6 property tests pass; all 47 `lib/tf/*.tf` files parse correctly; zero `cargo clippy` warnings
- [x] CI switched to Rust-primary with a feature matrix (default, `lua`, `python`); C build jobs disabled (`if: false`)
- [x] Binary renamed `tf-rust` → `tf` in `Cargo.toml`
- [x] `just run` updated to `cargo run --bin tf`; C legacy targets kept as `build-c`/`run-c`
- [ ] C source archived (optional — `src/` remains; move to `src-c/` at your discretion once Rust binary is in daily use)

---

## Post-Cutover Fixes ✓

Issues found during daily use and fixed after the Phase 15 cutover:

- [x] **Positional-arg expansion suite** — full TF expansion forms implemented: `{L}` (last param), `{-L}` (all-but-last), `{-N}` (all-but-first-N), `{name-default}`, `{N-default}`, `{*-default}`, `${name}` dollar-brace form, nested braces in defaults (`tf-rs/src/script/expand.rs`)
- [x] **`/load` expand-before-parse** — args are now expanded (resolving `%{-L}`, `%{L}` etc.) before flag parsing, matching C TF behavior
- [x] **TFLIBDIR search in `/load`** — bare filenames (no leading `/`, `.`, `~`) are searched in `TFLIBDIR` first, enabling `/require alias.tf` to resolve correctly
- [x] **`getopts(format[, defaults])`** — implemented as interpreter-aware builtin; parses `-X` flags from positional params, sets `opt_X` locals, replaces frame params with remaining args
- [x] **`ftime(format[, secs])`** — strftime-style UTC formatter via pure-Rust date decomposition; supports `%H %M %S %Y %y %m %d %e %j %A %a %B %b %p %I %w %n %t %%`
- [x] **`systype()`** — returns `"unix"` on all POSIX systems (was returning `"linux"`)
- [x] **`echo()`, `prompt()`, `substitute()`** — function-call forms wired to interpreter output buffer
- [x] **`%;` in `/def` body** — command separator no longer splits macro bodies at definition time

---

## Known Gaps

These features are recognized as missing but not yet scheduled.  Items are
grouped by impact so the highest-value work is obvious at a glance.

### Already resolved (archive)
- `/gag` — works via `/def -ag`; no standalone command needed
- `/hilite` / `/attr` — ✓ trigger `attr` field applied to `LogicalLine` on match
- `/purge [pattern]` — ✓ `MacroStore::purge()` removes anonymous or prefix-matched macros
- `/saveworld` — ✓ `World::to_addworld()` serializes to `/addworld` syntax
- `/beep` — ✓ writes `\x07` via `ScriptAction::Bell`
- `/visual`, `/mode`, `/redraw`, `/localecho` — ✓ explicit no-ops
- `/input` — ✓ `ScriptAction::SetInput` → `LineEditor::set_text()`
- `/status <format>` — ✓ `%world`/`%T`/`%t` tokens; flag-form silently accepted
- `/setenv` — ✓ `std::env::set_var`
- `/dokey` — ✓ `ScriptAction::DoKey`
- `/unset` — ✓ removes from globals
- `@{...}` TF attribute sequences — ✓ `TfStr::from_tf_markup()` parses all codes
- Raw mode + input line rendering — ✓ enabled at `EventLoop::run()` start
- Scrollback viewport anchor — ✓ `push_line()` keeps view pinned when scrolled back
- ATCP / GMCP — ✓ `Hook::Atcp` / `Hook::Gmcp`; 34 hook variants
- `mktime`, `ftime`, `textencode`, `textdecode`, `strchr`, `strrchr`, `regmatch`,
  `filename`, `dirname`, `isvar`, `ismacro` — ✓ implemented
- `kbpoint()` / `kbhead()` / `kbtail()` — ✓ live editor state via globals
- `moresize()` — ✓ `Screen::scrollback()` via globals
- `cputime()` — ✓ `getrusage(RUSAGE_SELF)`
- `status_fields()` / `status_width()` / `status_label()` — ✓ parse `%status_fields`
- `worldname()` / `nworlds()` — ✓ live event-loop state via globals
- Bare `tf` invocation with no world — ✓ starts idle cleanly

---

### High impact — missing, commonly needed in daily use

#### Commands
- ✓ `/sh [command]` — `sh -c <cmd>` output displayed on TF screen; bare `/sh` drops to `$SHELL` (leaves raw mode, waits, repaints)
- ✓ `/lcd [dir]` — `set_current_dir`; bare `/lcd` goes to `$HOME`; `~` prefix expanded; prints new cwd or error
- ✓ `/recall [n]` — displays last N (or all) input history entries oldest-first with `[N] text` numbering
- ✓ `/ps` — lists active processes with PID, type, interval, remaining runs, and description
- ✓ `/kill pid` — removes a process by ID; prints confirmation or "no such process"
- `/save [file]` — write current macro/trigger set to a `.tf` file; important for persistence
- `/unworld name` — remove a world definition from `WorldStore`
- `/listvar [pattern]` — list interpreter globals; debugging staple
- `/histsize n` — set scrollback buffer depth at runtime
- `/version` — print binary version string; scripts check `%version`

#### Functions
- `fg_world()` — `tfstatus.tf` uses `fg_world() =~ ""` to detect no active world;
  currently `worldname()` exists but `fg_world()` is a separate entry in `funclist.h`
  (they should return the same value)
- `is_open(world)` / `is_connected(world)` — check whether a named world has an
  open/established connection; used extensively in multi-world scripts
- `nactive()` — count of worlds with active (established) connections; used in status bars
- `columns()` / `winlines()` — terminal width and height; needed for layout calculations
  in `tfstatus.tf`, `activity_status.tf`, and user scripts
- `idle([world])` / `sidle([world])` — seconds since last keyboard input / server data;
  used in auto-away and watchdog scripts
- `tfopen(file, mode)` / `tfread(fd)` / `tfwrite(fd, str)` / `tfclose(fd)` /
  `tfflush(fd)` / `tfreadable(fd)` — file I/O API; used in logging, history-save,
  and data-export scripts

---

### Medium impact — useful but not daily blockers

#### Commands
- `/listsockets` — alias for `/listworlds`; some scripts use this name
- `/shift [n]` — drop first N positional params; used in argument-parsing macros
- `/undefn pattern` — bulk-remove macros matching a pattern (distinct from `/undef name`)
- `/histsize n` — already listed above (high)
- `/trigpc chance body` — fire body with given percent probability; used in some MUD scripts

#### Functions
- `fake_recv([world,] line)` — inject a line as if received from the server; invaluable
  for testing triggers and hooks without a live connection
- `nlog()` / `nmail()` / `nread()` — count of active log files / unread mail / unread
  lines; used in status bars from `tfstatus.tf`
- `world_info(world, field)` — query host/port/type/char for a named world
- `strip_attr(str)` — remove TF `@{...}` markup from a string, returning plain text
- `encode_ansi(str)` / `decode_ansi(str)` — convert between TF attr markup and raw ANSI
  escape sequences; used when passing strings between TF and shell commands
- `morepaused()` / `morescroll(n)` — query/control the pager; needed for `status_int_more`
  in `tfstatus.tf`
- `kblen()` / `kbdel(n)` / `kbgoto(n)` / `kbmatch([pat])` — remaining keyboard
  introspection and manipulation functions (complement to `kbpoint`/`kbhead`/`kbtail`)
- `gethostname()` — return the local hostname; used in scripts that tag log files

#### Display
- Full `tfstatus.tf` named-field system — `/status_add`, `/status_rm`, `/status_edit`,
  `/status_clear`; field-by-field evaluation of `status_int_*` / `status_var_*` exprs;
  dynamic (`@`) field refresh on a timer tick.  High effort; replaces the current
  `%world`/`%T`/`%t` token system entirely.

---

### Low impact — niche or rarely needed

- MCP (MUD Client Protocol) — complex, only a handful of servers use it
- `/features` — print compiled-in feature flags; trivial to add but rarely queried
- `/export name` — copy interpreter variable to `std::env`; `/setenv` covers most uses
- `/restrict level` — lock down dangerous commands; not needed for personal use
- `/suspend` — send `SIGSTOP` to self; job-control relic
- `/recordline str` — manually append a line to input history
- `/listsockets` — duplicate of `/listworlds`
- `/watchdog interval` / `/watchname name` — reconnect-on-silence; useful but
  implementable as a `/repeat` script
- `option102([world,] on/off)` — niche telnet option; only relevant to specific servers
- `isatty()` — check if stdin is a tty; rarely needed
- `keycode(key)` — look up raw escape sequence for a key name
