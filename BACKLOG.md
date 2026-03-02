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

## Proxy / Connection Gaps

### [P1] `/trigger -h<HOOK> args` — fire a hook directly at runtime — Task #20
**C source**: `command.c` (`handle_trigger_command`), `stdlib.tf:388-390`

`/trigger -hCONNECT ${world_name}` fires `H_CONNECT` immediately.  This is
called by `proxy_command` (via `proxy_hook`) right after sending the proxy
connect string, so that the normal CONNECT and LOGIN flows happen through a
proxy connection.  Without this, proxy connections never fire H_CONNECT or
H_LOGIN.

**Rust status**: `/trigger` in `interp.rs` only handles macro definitions; the
`-h` flag is not parsed.  **This breaks proxy support end-to-end.**

**Fix**: In `exec_builtin` for `/trigger`, detect `-h<name>` flag, resolve the
hook name via `Hook::from_str`, and call `self.actions.push(ScriptAction::FireHook(hook, args))`.
Add a `ScriptAction::FireHook` variant handled in `EventLoop::handle_script_action`.

---

### [P2] `/connect host port` — direct connection creating a temporary world
*(see also [C7] below — same item)*
**C source**: `socket.c:1293-1321`, `command.c:151-177` — **Task #23**

`/connect hostname 4000` (two args) creates a `WORLD_TEMP` world and connects.
Currently Rust treats the first argument as a named world and fails with
"Unknown world 'hostname'".

**Fix**: In `connect_world_by_name` (or a new handler), detect when the name
looks like a host (or when two args are given), create a temporary `World` with
`flags.temp = true`, add it to `self.worlds`, and connect.

---

### [P3] `/fg` flags not parsed
*(see also [C8] below — same item)*
**C source**: `socket.c:1241-1284` — **Task #23**

C supports: `-n` (background/no-socket), `-q` (quiet), `-c<n>` (Nth socket),
and relative `+`/`-<n>` cycling.  Used by `dokey_socketf`/`dokey_socketb` in
`kbfunc.tf` via `/fg -c$[-kbnum?:-1]`.

**Rust status**: `interp.rs:647-655` treats all args as a world name — flags
are silently ignored, so world cycling is broken.

**Fix**: Parse `-c<n>` (index into `self.handles` keyset) and `-n`/`-q` flags
before falling through to name lookup.

---

### [P4] `world_info()` missing fields — Task #24
**C source**: `socket.c:4044-4081`

C `world_info(worldname, field)` supports 11 fields.  Rust only handles 5
(`host`, `port`, `type`, `character`/`char`, `mfile`).  Missing: `name`,
`password`, `login`, `proxy`, `src`, `cipher`.

**Fix**: Add the missing arms to the `world_info` match in `interp.rs`.
`"login"` → `world_login` global; `"proxy"` → check if world name is in
`self.handles`; others from `World` struct fields.

---

### [P5] `nactive(worldname)` ignores argument — Task #24
**C source**: `socket.c:4110-4119`

C returns per-world new-message count for a specific world when an arg is
given.  Rust always returns total open world count from the `nactive` global.

**Fix**: Parse optional arg; if present, look up per-world activity count.
(Requires tracking per-world unread line count, which we currently don't.)

---

### [P6] `TFPROXY` environment variable not read
**C source**: `variable.c` (proxy_host defined in `varlist.h`)

C TF reads `proxy_host`/`proxy_port` as regular settable variables (set via
`/set proxy_host=...` in `.tfrc`).  The `TFPROXY` env var is not explicitly
read by C either — users set it via `/set`.  **Not actually a gap** — our
`get_global_var("proxy_host")` lookup works correctly once the user does
`/set proxy_host=...` in their `.tfrc`.

*(Kept here as a note to avoid re-investigating.)*

---

## Missing / Broken Functions

### [F5] `morescroll(n)` not implemented — Task #22
**C source**: `expr.c:1160` — scrolls the paged-output buffer by n lines; returns lines cleared.
Used by `/dokey_page`, `/dokey_pageback`, and any paging script.
**Rust status**: Not present in `builtins.rs`. PgUp/PgDn keybindings that call `morescroll()` silently fail.
**Fix**: Add to `call_fn`; queue `ScriptAction::Scroll(n)` → handled in EventLoop as `Screen::scroll`.

---

### [F6] `morepaused()` not implemented — Task #26
**C source**: `expr.c:1155` — returns 1 if the active screen is paused in paged-output mode.
**Rust status**: Not present in `builtins.rs`. Returns nothing (treated as `""`).
**Fix**: Add to `call_fn`; return `self.interp.get_global_var("_morepaused")` (already maintained).

---

### [F7] `nmail()`, `nread()` not implemented — Task #26
**C source**: `expr.c:1549-1553`.  `nmail()` = new mail count; `nread()` = unread mail count.
**Rust status**: `nlog()` IS correctly implemented via the `nlog` global. `nmail`/`nread` are not.
Rarely used in practice but listed in funclist.h.
**Fix**: Stub returning 0; add `nmail` and `nread` globals defaulting to 0.

---

### [F8] `prompt(text)` — just prints instead of setting the input prompt — Task #21
**C source**: `expr.c:962-963` — sets the displayed prompt string on the input line.
**Rust status**: `interp.rs:1985-1988` pushes `"[prompt] {text}"` to output — completely wrong semantics.
**Fix**: Queue `ScriptAction::SetPrompt(text)`; EventLoop stores it and renders it left of the input line.

---

### [F9] `send()` flag `"h"` — SEND hook not fired — Task #26
**C source**: `send()` flags: `"u"` = no newline, `"h"` = fire SEND hook after sending.
**Rust status**: `interp.rs:1820-1830` handles `"u"` but not `"h"`. The SEND hook is never fired by `send()`.
**Fix**: If flags contains `"h"`, push `ScriptAction::FireHook(Hook::Send, text)` after the send action.

---

## Missing Commands / Flag Gaps

### [C7] `/connect host port` — direct connect without a named world — Task #23
**C source**: `command.c:151-177` — two-arg form creates a `WORLD_TEMP` anonymous world.
**Rust status**: Treated as a single world name; fails with "Unknown world 'host'".
**Fix**: In the Connect action handler, if the arg contains a space or port is given separately, create a temporary World and connect.

---

### [C8] `/fg` flags not parsed — Task #23
**C source**: `socket.c:1241-1284` — `-n` (no-world), `-q` (quiet), `-c<n>` (absolute index), `+`/`-<n>` (relative cycle).
Used by `dokey_socketf`/`dokey_socketb` in `kbfunc.tf` as `/fg -c$[-kbnum?:-1]`.
**Rust status**: All args treated as a world name; cycling is broken.
**Fix**: Parse flags before the name lookup in `connect_world_by_name`.

---

### [C9] `/connect` flags `-f`/`-b` (foreground/background) not parsed — Task #23
**C source**: `command.c:151` — `-f` brings connection to foreground, `-b` keeps it in background.
**Rust status**: Not parsed; all new connections become foreground.
**Fix**: Parse flags in the Connect handler; set active_world only when `-f` or no flag.

---

## Environment / Variable Init Gaps

### [E1] `LANG`, `LC_ALL`, `LC_CTYPE`, `LC_TIME`, `TZ` not initialized from environment — Task #25
**C source**: `varlist.h:32-42` — C TF defines these as auto-export variables, initialized from env.
**Rust status**: `main.rs` only sets a hardcoded list of defaults. These vars are never read from `std::env`.
**Fix**: On startup, for each of LANG, LC_ALL, LC_CTYPE, LC_TIME, TZ: read `std::env::var()` and call `set_global_var` if set.

---

### [E2] `MAIL`, `TERM` not initialized from environment — Task #25
**C source**: `varlist.h:37-38` — initialized from environment for mailbox monitoring and terminal type.
**Rust status**: Never set.
**Fix**: Same as E1 — read from env on startup.

---

### [E3] `TFPATH` not initialized — Task #25
**C source**: `varlist.h:41` — search path for macro files.
**Rust status**: Never set. `/load` without a path prefix won't search `TFPATH`.
**Fix**: Read `TFPATH` env var on startup; store as `path`-typed global.

---

## Subtle Semantic Correctness (implemented but possibly wrong)

### [Q1] `%gag` variable not actually suppressing output lines — Task #28
**Priority**: High — users expect `/gag pattern` to silence output; if the flag
isn't checked in the render pipeline, matching lines still appear.

C TF checks `gag` flag on each trigger-matched line and skips
`oprintf`/screen push.  Rust sets the `gag` variable and trigger `attr.gag`
bit, but the EventLoop output path may not check the global `%gag` variable
for lines that *don't* match any trigger.

**Fix**: In `handle_net_message`, after trigger matching, skip `screen.push_line`
if `gagged || self.interp.get_global_var("gag").map(|v| v.as_bool()).unwrap_or(false)`.

---

### [Q2] `%hilite` and trigger attribute merging — Task #29
**Priority**: High — multiple `/hilite`/`/def -ah` rules on the same line should
OR their attributes together (bold from one + red from another both apply).

Two sub-issues:
1. When multiple triggers match, Rust may only apply the last match's attrs
   rather than folding all of them.
2. The global `%hilite` flag (when 0) should disable attribute application
   entirely; when 1 it enables it.  Not certain Rust checks this flag.

**Fix**: Audit `find_triggers` return and the attr-fold in `handle_net_message`;
ensure `merged_attr = actions.iter().fold(Attr::EMPTY, |a, t| a | t.attr)`.
Check global `%hilite` before applying.

---

### [Q3] Trigger `-c<n>` self-destruct count not decremented — Task #33
**Priority**: Medium — `/def -c3 pattern = body` should fire 3 times then
auto-remove.  We likely parse the count but never decrement/remove.

**C source**: `macro.c` — each trigger invocation decrements `m->nfields` (the
count) and removes the macro when it hits 0.

**Fix**: After firing a trigger, decrement its count field; if it reaches 0,
push `ScriptAction::Undef(name)`.

---

### [Q4] Macro priority tiebreak ordering — Task #36
**Priority**: Low — when two triggers have equal priority, C TF fires them in
definition order (most recently defined first).  Our sort may differ.

**Fix**: Audit `MacroStore::find_triggers` sort key; add a definition-order
sequence number as a tiebreak.

---

## Connection Lifecycle Gaps

### [X1] `/dc` (disconnect) may not fire H_DISCONNECT or switch active world — Task #30
**Priority**: High — C TF's disconnect fires `H_DISCONNECT`, marks the socket
zombie, then switches to another world if the active world disconnected.

**Rust status**: Unclear whether `H_Disconnect` hook fires and whether
`active_world` is updated after a `/dc`.

**C source**: `socket.c` — `do_hook(H_DISCONNECT, ...)`, `fg_sock(NULL)`.

**Fix**: Audit the Disconnect ScriptAction handler in EventLoop; ensure hook
fires, handle is removed, and active_world switches to next available.

---

### [X2] `nactive` counts open handles instead of worlds with unread output — Task #31
**Priority**: Medium — C TF's `nactive` is the count of background worlds that
have received new text since you last viewed them — it's what drives the
`(Active)` status bar field.  If you're on world A and world B gets output,
`nactive` increments.

**Rust status**: `nactive` global is set to `self.handles.len()` — always equal
to the number of open connections, never decreasing.

**Fix**: Track a `unread_worlds: HashSet<String>` in EventLoop; add to it when
a background world gets a line; clear it when `/fg`-ing to that world.  Set
`nactive` from its length.

---

### [X3] Per-world mfile not re-sourced on `/fg` switch — Task #36
**Priority**: Low — C TF's `sockmload` variable controls whether a world's
`mfile` macro file is re-sourced every time you foreground that world.  Useful
for world-specific keybinds/triggers that should refresh on switch.

**C source**: `socket.c:1229` — `if (sockmload) wload(sock->world)`.

**Fix**: In the `/fg` handler, after switching active world, check `%sockmload`
global; if set, source `world.mfile` if present.

---

## Hollow / Stub Subsystems

### [H1] `/more` paging does not actually pause output — Task #32
**Priority**: High — C TF pauses rendering and waits for a keypress when output
fills the screen (`%more=1`).  We have the `%more` variable and stubs for
`/limit`/`/relimit`/`/unlimit` but output just scrolls past.

**Fix**: In `Screen::push_line`, track line count since last user interaction;
when it exceeds `winlines`, set a `more_paused` flag and stop rendering until
the user presses a key (similar to `less`).  Wire `_morepaused` global to this.

---

### [H2] `/recall` command flags incomplete — Task #33
**Priority**: Medium — `/recall` as a command accepts flags: pattern match,
world filter (`-w`), count (`-n`), direction (`-b` backwards).  Keyboard
up/down arrow history works, but the command form may be a stub or partial.

**C source**: `history.c` — full flag parsing.

**Fix**: Audit `/recall` in `exec_builtin`; implement `-n`, `-w`, `-b`, and
pattern-filter forms.

---

### [H3] `/save` may not reconstruct full session state — Task #33
**Priority**: Medium — C TF's `/save [file]` writes `/def`, `/addworld`,
`/set` statements to reconstruct the current macro/world/variable state.  If
ours only saves worlds or does nothing useful, config is lost between sessions.

**Fix**: Audit `/save` in `exec_builtin`; ensure it writes `/addworld` for each
world, `/def` for each non-stdlib macro, and `/set` for user-modified variables.

---

## Scripting Language Gaps

### [G1] `@@var` indirect variable expansion not implemented — Task #34
**Priority**: High — `@@varname` expands to the value of the variable *named
by* `%varname`.  Used in advanced dispatch patterns.  Almost certainly not
implemented in our expander.

**C source**: `expand.c` — handles `@@` prefix specially.

**Fix**: In the variable-expansion path in `interp.rs`, detect `@@name`; look
up `%name`, then look up `%{value-of-name}`.

---

### [G2] `%()` inline expression form — Task #34
**Priority**: High — `%(expr)` evaluates an expression inline during string
expansion, distinct from `$[expr]`.  Both are used in `lib/tf/*.tf`.

**C source**: `expand.c`.

**Fix**: In the string expander, detect `%(` and eval the contained expression,
substituting the result.  Audit whether this is already handled or silently
dropped.

---

### [G3] `/shift` command — Task #34
**Priority**: High — shifts positional arguments left: `{2}` becomes `{1}`, etc.
Used in multi-arg macro dispatch.

**C source**: `command.c` handle_shift_command.

**Fix**: In `exec_builtin`, implement `"shift"`: remove `args[0]` from the
current frame's positional arg list and renumber.

---

### [G4] `/result` command — Task #34
**Priority**: High — retrieves the return value of the last `/test` expression
as a string.  Distinct from running `/test` again.

**C source**: `command.c` — reads `last_result` global.

**Fix**: Store expression results in `interp.last_result`; `/result` returns it.

---

## Testing Infrastructure

### [T1] Non-interactive / batch mode (stdin not a tty)
**Priority**: High — prerequisite for the test harness. — **Task #37**

C TF runs non-interactively when stdin is not a tty: `%visual=0`,
`%interactive=0`, no raw mode, output goes straight to stdout, exits on stdin
EOF.  Combined with `-n` this gives a clean scripting sandbox:

```bash
printf '/test 1+2\n/exit\n' | tf -n
```

Rust currently calls `Terminal::enter_raw_mode()` unconditionally; this will
fail or behave incorrectly without a tty.

**Fix**: When `!isatty(stdin) || !isatty(stdout)`, skip `enter_raw_mode()`, skip
the TUI render loop, write output lines directly to stdout, read commands from
stdin line-by-line, exit cleanly on EOF.

---

### [T2] C-vs-Rust script test harness
**Priority**: High — **Task #38** (depends on T1)

A shell script or `cargo test` integration that pipes the same `.tf` commands
to both the C and Rust binaries, normalises output (strip ANSI codes, timing),
diffs the results, and reports mismatches.

Start with hand-written cases covering: expressions, string functions,
conditionals, loops, variable expansion edge cases (`@@var`, `%()`), positional
args, trigger matching.

---

### [T3] Community TF script corpus
**Priority**: Medium — **Task #39** (depends on T2)

Collect `.tf` scripts from public archives (GitHub, MUD community sites, the
TF mailing list) and run them through the C-vs-Rust harness.  Focus on scripts
that exercise pure scripting (no network) so they work with `-n`.

---

## Low Priority / Polish

### [L1] Startup message order doesn't match C TF
**Task #27**

The order of lines displayed during startup (version banner, locale messages,
"Loading commands from ...", auto-connect output) does not match the C binary.
Very low priority — functionally equivalent, just cosmetically different.

**Fix**: Compare the C startup sequence in `main.c` step by step against
`main.rs` and reorder the `push_line` / `println!` calls to match exactly.

---

### [L2] Terminal exit behavior doesn't match C TF
**Task #36** (bundled with other low-priority items)

C TF clears the screen on exit and places the shell prompt at the top.  The
Rust binary uses a different approach (clears status/input rows, leaves output
visible).  Deferred as "maybe" — the Rust behaviour may actually be preferable.

**Fix**: In `Terminal::cleanup()`, optionally clear the full screen and move
the cursor to row 0 before disabling raw mode.

---

### [H4] `/help` output doesn't match C TF
**Priority**: Medium — **Task #35**

The `/help` system produces significantly different output from C TF.  C TF
uses a compiled index file (`tf-help.idx`) mapping topic names to byte offsets
in a help document.  We embed `tf-help.idx` but never query it.

**Fix**: Implement `/help` to search the embedded index and extract the relevant
section, matching C TF's topic-lookup format.

**C source**: `command.c` handle_help_command, `makehelp.c`.

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

### [P1] Investigate scripting performance: tree-walking vs bytecode VM — Task #20

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
