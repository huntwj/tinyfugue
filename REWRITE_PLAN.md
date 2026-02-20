# TinyFugue → Rust Rewrite Plan

## Philosophy

This is a gradual, *strangler-fig* rewrite: the C binary continues to work throughout. A parallel Rust binary grows phase by phase until it reaches feature parity, at which point the C code is retired. No big-bang cutover.

**Port order follows the dependency graph**: leaf modules (no internal deps) first, core event loop last.

---

## Phase 0: Scaffolding (current)

**Goal**: Rust workspace exists, builds, and is integrated into the project tooling.

- [x] Create `rust/` Cargo workspace with a `tf` binary crate
- [x] Add `just build-rust` and `just run-rust` tasks
- [x] Add Rust build to CI (`build.yml`)

---

## Phase 1: Core Data Types

**C source**: `dstring.c`, `attr.c`, `search.c`, `malloc.c`

**Goal**: Establish the foundational types that all other modules depend on.

- `Str` — owned, growable string with optional per-character attribute vector (replaces `String`/`conString` + `charattrs`)
- `Attr` — text attribute flags (bold, underline, fg/bg color) as a Rust `bitflags!` type
- Generic `List`, `HashTable` wrappers around `std` collections
- No external crate dependencies in this phase

**Acceptance criteria**: unit tests covering string growth, attribute encoding/decoding, and basic collection operations.

---

## Phase 2: Pattern Matching

**C source**: `pattern.c` (wraps PCRE2)

**Goal**: Encapsulate regex behind a `Pattern` trait so the backing engine can be swapped.

- `Pattern` struct wrapping the `regex` crate
- Named capture groups, case-insensitive matching, substring extraction
- Gag/highlight attribute attachment per match

**Crates**: `regex`

---

## Phase 3: World & Configuration Model

**C source**: `world.c`, `variable.c`

**Goal**: Data model for MUD connections and user-configurable variables.

- `World` struct: host, port, character, SSL flag, type, associated macros
- `Variable` store: global and per-world key/value settings
- `.tfrc` config file parsing (enough to load world definitions)

---

## Phase 4: TF Scripting Language

**C source**: `expand.c`, `expr.c`, `parse.h`, `opcodes.h`, `command.c`, `cmdlist.h`

**Goal**: A working TF script interpreter. This is the largest single-phase effort.

- Lexer + recursive descent parser producing an AST
- Bytecode compiler (opcodes: arithmetic, string ops, control flow, variable access)
- Stack-based VM with `Value` enum (`Int`, `Float`, `Str`, `Void`)
- Built-in commands: `/send`, `/set`, `/let`, `/if`, `/while`, `/return`, `/echo`, etc.
- Variable substitution (`%var`, `{macro}`, positional args `{1}` … `{#}`)

**Note**: Reach semantic equivalence with the C implementation before optimising. The test suite for this phase should run existing `.tf` script files from `lib/tf/`.

---

## Phase 5: Macro & Trigger System

**C source**: `macro.c`, `hooklist.h`, `enumlist.h`

**Goal**: The trigger/hook engine that connects server output to TF scripts.

- `Macro` struct: name, body, trigger pattern, key binding, priority, world scope
- Hook dispatch table (CONNECT, DISCONNECT, ACTIVITY, GMCP, …)
- Priority-ordered pattern matching against incoming lines
- Gag, highlight, and per-match attribute application

---

## Phase 6: Terminal Output

**C source**: `output.c`, `tty.c`

**Goal**: Render MUD output and the status line to the terminal.

- `Screen` abstraction: logical lines, physical (wrapped) lines, scrollback
- ANSI attribute rendering from `Attr` values
- "More" pagination mode
- Status line with configurable format and clock
- `crossterm` for cross-platform terminal control (replaces termcap)

**Crates**: `crossterm`

---

## Phase 7: Keyboard & Input Handling

**C source**: `keyboard.c`, `keylist.h`

**Goal**: Read and edit user input, dispatch key bindings.

- Readline-style line editor (cursor movement, kill/yank, history recall)
- Key binding lookup against the macro table
- Word-boundary navigation respecting `wordpunct`

---

## Phase 8: Networking & Telnet

**C source**: `socket.c` (~4,000 lines), `tfselect.h`

**Goal**: Async multi-connection TCP client with Telnet protocol support.

- `tokio`-based async I/O replacing `select()` (one task per socket)
- Telnet option negotiation FSM: NAWS, CHARSET, TTYPE, ECHO
- Protocol extensions: ATCP, GMCP, option 102
- SSL/TLS via `rustls` (replaces OpenSSL)
- MCCP decompression via `flate2` (replaces zlib direct)

**Crates**: `tokio`, `rustls`, `flate2`

---

## Phase 9: Main Event Loop & Signal Handling

**C source**: `main.c`, `signals.c`, `process.c`, `timers.c`

**Goal**: The top-level runtime tying all subsystems together.

- `tokio::select!`-based loop over: keyboard input, socket activity, timers, signals
- `/quote` and `/repeat` process scheduling
- SIGWINCH (terminal resize) and SIGTERM/SIGINT handling
- Mail check timer

---

## Phase 10: Optional Embedded Scripting

**C source**: `lua.c`, `tfpython.c`

**Goal**: Restore optional Lua and Python embedding in the Rust binary.

- Lua via `mlua` crate
- Python via `pyo3` crate
- Same `/calllua` and TF↔script bridging API as the C version

**Crates**: `mlua` (optional feature), `pyo3` (optional feature)

---

## Phase 11: Cutover

- Feature parity verified against the C test suite and manual testing
- CI switched to build only the Rust binary
- C source archived (or removed)
- `just run` updated to run `tf-rust`
