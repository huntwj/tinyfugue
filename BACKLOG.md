# tf-rs Backlog

Items identified by auditing the C TF source against the Rust rewrite.
Goal: match C TF behaviour wherever possible before considering new changes.

Items are sorted by impact within each section.  Cross-references to C source
are provided so implementers can read the original behaviour.

---

## Startup / Behavioural Parity

### [S1] Set `%visual` and `%interactive` at startup
**C source**: `main.c:201-207`

C sets these after loading the config file:
```c
if (getintvar(VAR_visual) < 0 && !no_tty)
    set_var_by_id(VAR_visual, autovisual);   // 1 normally, 0 with -v flag
if (getintvar(VAR_interactive) < 0)
    set_var_by_id(VAR_interactive, !no_tty); // 1 if stdin is a tty, else 0
```
`no_tty` is `!isatty(STDIN_FILENO) || !isatty(STDOUT_FILENO)`.

In Rust: `%visual` and `%interactive` are never set; the `-v` flag is parsed
(`args.no_visual`) but ignored.  Many scripts check these variables.

**Fix**: After loading the user config, detect `isatty`, then set
`%interactive = 1|0` and `%visual = (no_visual ? 0 : 1)|0` in
`event_loop.interp`.

---

### [S2] Fire the WORLD hook on connect / switch / no-world
**C source**: `socket.c:1169-1175, 1223, 1233, 1308`

```c
world_hook("---- World %s ----", world_name);   // on connect/switch
world_hook("---- No world ----", NULL);          // when -n or no default world
```
`world_hook()` fires `H_WORLD` (our `Hook::World`) with the formatted string as
the argument and also pushes it to the display.

In Rust: `Hook::World` exists in the enum but is never fired.  The "---- World
..." divider line that TF users rely on to see which world is active is absent.

**Fix**:
- In `EventLoop::connect_world_by_name`, after a successful connect, call
  `self.fire_hook(Hook::World, &format!("---- World {name} ----")).await`
  and push the same string to the screen.
- In `ScriptAction::SwitchWorld` handler in `handle_script_action`, fire the
  hook with the world name.
- In `main.rs`, when `-n` is passed (or `ConnectTarget::Default` finds no
  world), push "---- No world ----" to the screen and fire `Hook::World`.

---

### [S3] Apply `-l` (no autologin) and `-q` (quiet login) flags
**C source**: `main.c:93-94, 211-214`

Both flags are parsed in Rust's `cli.rs` but never used.  In C they set
`CONN_AUTOLOGIN` / `CONN_QUIETLOGIN` bits passed to `openworld()`, which
controls whether the LOGIN hook fires and whether connect messages are shown.

**Fix**: Thread `args.no_autologin` and `args.quiet_login` through to
`connect_world_by_name`.  Store them on `EventLoop`; suppress the LOGIN hook
(and/or quiet the connect output) when the respective flag is set.

---

### [S4] Early `puts()` banner to stdout before screen init
**C source**: `main.c:81-84`

Before any argument parsing, C prints to stdout:
```c
puts("");
puts(version);
puts(mods);
puts(copyright);
```
This appears in the terminal *before* the TF UI starts (useful when stdout is
redirected or when the terminal is slow to initialise).

In Rust we only push the banner into the scrollback buffer (visible after
`run()` starts).  When stdout is not a tty this output is lost entirely.

**Fix**: Add matching `println!()` calls in `main.rs` before `EventLoop::new()`.

---

### [S5] `/version` command — include mods / contrib / platform
**C source**: `command.c:273-281`

```c
oprintf("%% %s.", version);
oprintf("%% %s.", copyright);
if (*contrib) oprintf("%% %s", contrib);
if (*mods)    oprintf("%% %s", mods);
if (*sysname) oprintf("%% Built for %s", sysname);
```
Rust only prints version and copyright.

**Fix**: Add a `mods` constant (e.g. "Rust rewrite") and `sysname`
(`std::env::consts::OS`) and include them in the `/version` output.

---

## Missing Built-in Functions

### [F1] `send(text[, world[, flags]])` — send text to a world
**C source**: `socket.c:handle_send_function`, `funclist.h`

This is the **primary way scripts send text to a MUD**.  `stdlib.tf:113` calls
it directly:
```tf
/test send(_text, {opt_w}, _flags)
```
Flags: `"u"` = no trailing newline (unflushed/raw), `"h"` = fire SEND hook.

In Rust: `call_builtin` has no `"send"` arm.  The `/send` stdlib macro silently
fails when called from a script context.

**Fix**: Add `"send"` to `call_builtin` in `builtins.rs`.  It should push a
`ScriptAction::SendToWorld` (with an optional no-newline variant) — but
`call_builtin` cannot queue actions.  Best approach: handle it in
`Interpreter::call_fn` (which has `&mut self`) rather than the pure
`call_builtin`, queuing `SendToWorld { text, world, no_newline }`.

---

### [F2] `kbwordleft([n])` and `kbwordright([n])` — word-jump cursor moves
**C source**: `funclist.h`, `keyboard.c`

Move the input-line cursor left/right by `n` words (default 1).  Commonly
bound to `M-b` / `M-f` in user key maps.

**Fix**: Add `DoKeyOp::WordLeft` / `DoKeyOp::WordRight` variants to the
`DoKeyOp` enum, implement the motion in `LineEditor`, and wire the functions in
`call_builtin`/`interp.rs` to queue `ScriptAction::DoKey(op)`.

---

### [F3] `strcmpattr(s1, s2)` — compare strings ignoring display attributes
**C source**: `funclist.h`, `tfstr` attribute layer

Compares two TfStr values lexicographically, ignoring colour/bold/etc.
In Rust: not implemented.  Rarely called by user scripts but listed in the
function table.

**Fix**: Add to `call_builtin`; strip attrs and delegate to normal string
comparison.

---

### [F4] `read()` — read a line from stdin
**C source**: `funclist.h`, `tfio.c`

Reads one line from stdin (used in batch/non-interactive scripts).  Not needed
in interactive use.

**Fix**: Add to `call_builtin`; use `std::io::stdin().read_line()`.  Return
the line (without trailing newline) or empty string on EOF.

---

## Missing Commands

### [C1] `/gag [pattern]` — gag/suppress matching output
**C source**: `command.c:handle_gag_command`, `cmdlist.h`

With no args: sets `%gag = 1` (enable gagging globally).
With a pattern: shorthand for `/def -ag pattern` (create a gag trigger).

**Fix**: In `exec_builtin`:
- No args: `self.set_global_var("gag", Value::Int(1))`.
- With args: parse pattern and push `ScriptAction::DefMacro` with the gag
  attribute flag set.

---

### [C2] `/hilite [pattern]` — highlight matching output
**C source**: `command.c:handle_hilite_command`, `cmdlist.h`

With no args: sets `%hilite = 1`.
With a pattern: shorthand for `/def -ah pattern` (create a hilite trigger).

**Fix**: Mirror `/gag` implementation with the hilite attribute flag.

---

### [C3] `/relimit` and `/unlimit` — scroll-limit control
**C source**: `cmdlist.h`, `output.c`

`/limit` enables paged output (already stubbed).
`/relimit` re-enables limiting after it was hit.
`/unlimit` removes the limit entirely.

**Fix**: These are currently stubs.  Implement basic paged-output limit
tracking on `Screen` and wire these three commands to it.

---

### [C4] `/core` — dump debug information
**C source**: `command.c:handle_core_command`

Prints internal debug state.  Rarely used outside development.

**Fix**: Stub that prints a short message ("not implemented in Rust build") or
implement as a debug dump of interpreter state.

---

### [C5] `/edit` — edit input in external `$EDITOR`
**C source**: `command.c:handle_edit_command`

Opens the current input-line content in `$EDITOR`, then re-inserts on exit.
Requires leaving raw mode, running the editor, re-entering raw mode.

**Fix**: `ScriptAction::EditInput`; event loop writes buffer to a temp file,
runs `$EDITOR`, reads back, calls `self.input.set_text(...)`.

---

### [C6] `/liststreams` — list open tfopen file descriptors
**C source**: `cmdlist.h`, `tfio.c`

Displays all file descriptors opened via `tfopen()`.

**Fix**: Iterate `self.interp.tf_files` and print fd → path/mode.

---

## Variable Initialisation Gaps

The following variables are defined in C's `varlist.h` and consulted by
`stdlib.tf` or user config files, but are never set in the Rust binary.
Most can be stubbed as reasonable defaults.

| Variable | Default | Notes |
|---|---|---|
| `%visual` | 1 (tty) / 0 (no tty) | See [S1] |
| `%interactive` | 1 (tty) / 0 (no tty) | See [S1] |
| `%quiet` | 0 | Set by `-q` flag; see [S3] |
| `%gag` | 0 | Global gag on/off; see [C1] |
| `%hilite` | 1 | Global hilite on/off; see [C2] |
| `%scroll` | 1 | Scroll-on-output flag |
| `%wrap` | 1 | Word-wrap flag |
| `%login` | 1 | Autologin default |
| `%sub` | `"both"` | Substitution mode |
| `%more` | 1 | Paged-output default |

---

## Already Resolved (do not re-add)

- [x] Startup banner pushed to scrollback — fixed in `main.rs`
- [x] Auto-connect always attempted (was guarded by `has_default_world()`) — fixed
- [x] `/restrict`, `/suspend`, `option102` — implemented
- [x] All P1 / P2 items from original REWRITE_PLAN.md
- [x] `/set` and `/let` — handled as `Stmt::Set` / `Stmt::Let` at parse time,
  not via exec_builtin; correctly implemented
- [x] `send` command form — text sent by typing at the prompt goes through
  `SendToWorld`; the `/send` *command* itself is a stdlib.tf macro
- [x] [S4] Early `puts()` banner to stdout — added `println!()` calls before arg parsing
- [x] [S1] `%visual` and `%interactive` set after config loading using `libc::isatty()`
- [x] [S2] `Hook::World` fired on connect, switch, and `-n` (no world) cases
- [x] [S5] `/version` output enhanced with mods string and platform (`std::env::consts::OS`)
- [x] Variable defaults set before stdlib load: `%quiet`, `%gag`, `%hilite`, `%scroll`,
  `%wrap`, `%login`, `%sub`, `%more`
- [x] [F1] `send(text[, world[, flags]])` — implemented in `call_fn`; added `no_newline`
  field to `ScriptAction::SendToWorld`; `"u"` flag uses `send_raw` (no CRLF)
- [x] [F2] `kbwordleft([n])` / `kbwordright([n])` — implemented in `call_fn` using
  `%kbhead`/`%kbtail` globals; Emacs M-b/M-f word-boundary model
- [x] [F3] `strcmpattr(s1, s2)` — strips `@{...}` markup then delegates to strcmp
- [x] [F4] `read()` — reads one line from stdin via `std::io::stdin().lock().read_line()`
- [x] [C1] `/gag [pattern]` — no args sets `%gag=1`; with pattern calls `/def -ag`
- [x] [C2] `/hilite [pattern]` — no args sets `%hilite=1`; with pattern calls `/def -ah`
- [x] [C3] `/relimit` / `/unlimit` — stubs that set `%more` 1/0
- [x] [C4] `/core` — stub that prints "not implemented in Rust build"
- [x] [C5] `/edit` — opens input buffer in `$EDITOR` (or `$VISUAL`/`vi` fallback),
  writes to a temp file, re-inserts result on exit; mirrors C TF's handle_edit_command
- [x] [C6] `/liststreams` — lists `self.tf_files` by fd and mode (r/w/a)
- [x] [S3] `-l` no autologin / `-q` quiet login — `Hook::Login` fires after connect with
  `"world character password"` when `!no_autologin` and world has credentials; `no_autologin`
  and `quiet_login` fields on `EventLoop` threaded from CLI args; `parse_def` hargs bug fixed
  (was consuming macro name as hargs pattern — now parsed from `/pattern` suffix in spec only)

---

## Performance

### [P1] Investigate scripting performance: tree-walking vs bytecode VM

The C TF interpreter (`expr.c`) compiles macro bodies into a bytecode
representation and runs them via a simple VM.  The Rust rewrite
(`tf-rs/src/script/`) uses a tree-walking interpreter over an in-memory AST.

Tree-walking is simpler to implement but may be considerably slower for
macros that are invoked frequently (triggers, hooks, aliases) or that have
complex bodies with loops and conditionals.

**Investigation tasks:**

1. Profile real workloads (e.g. a trigger-heavy mudding session) to quantify
   the gap — it may be negligible given I/O dominates most MUD traffic.
2. If the gap is significant, consider:
   - Compiling the AST to a simple bytecode (stack machine) before first
     execution and caching the bytecode on the `Macro` struct.
   - Alternatively, evaluate whether an existing Rust bytecode VM crate
     (e.g. `piccolo`, `luster`) is suitable.
3. Compare `parse_def` re-parse overhead: currently each `/def` re-parses
   the body on every invocation (H9 in CODE_REVIEW.md); caching the AST is
   a prerequisite for bytecode compilation.
