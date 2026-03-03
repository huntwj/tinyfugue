# tf-rs Backlog

Open items only.  Completed work is in the **Already Resolved** section at
the bottom.  Priority ordering reflects lessons from real script testing:
the compat harness and community-script analysis (tf-util, tf-wotmud, etc.)
are now the primary source of truth for what matters.

---

## HIGH — Scripting Language: Missing Functions & Commands

These gaps were discovered by loading real user scripts (huntwj/tf-util,
huntwj/tf-wotmud).  `tf-util` is a foundational library that the other
scripts depend on; it fails to initialise without `strcat`, `textencode`,
and `regmatch`.

### [G5] `strcat(a, b, ...)` — string concatenation function
Used everywhere in tf-util (events.tf, variables.tf, list-macros.tf).
The `.` expression operator covers `$["a"."b"]` but `strcat` is called
directly via `/test strcat(a, b)`.

**Fix**: Add `"strcat"` to `call_fn`; fold all args into a single `String`.

---

### [G6] `textencode(s)` / `textdecode(s)` — encode/decode for variable names
tf-util encodes event names and callbacks as variable-name suffixes.
Without these, the entire event system and anything that builds on it
(watchVar, triggers, etc.) fails.

**C source**: `funclist.h`, `tfio.c` — percent-encodes whitespace and
punctuation so any string can be embedded in a TF variable name.

**Fix**: Add both to `call_fn`.  A minimal implementation:
`textencode` → replace each non-alphanumeric char with `%XX` hex.
`textdecode` → reverse the `%XX` → char mapping.

---

### [G7] `regmatch(pattern, str [, repl])` — PCRE match + capture groups
tf-util uses `regmatch("^\s*$", val)` as an "is blank?" predicate and
captures substrings via `%P1`…`%Pn` after a match.

**C source**: `funclist.h`, `pattern.c` — runs PCRE2, sets `%P0`…`%Pn`
globals from subgroups; returns 1 on match, 0 otherwise.

**Fix**: Add to `call_fn`; use the existing `Pattern::Regexp` engine, set
`%P0`…`%Pn` in interpreter globals, return 0/1.

---

### [G8] `/eval cmd` — execute a dynamically built command string
tf-wotmud builds `/def -mregexp -h'PROMPT …'` strings at runtime and
runs them with `/eval`.  Without this, dynamic trigger registration fails.

**C source**: `command.c` `handle_eval_command` — expands its argument as
a string then dispatches the result as a TF command.

**Fix**: Add `"eval"` to `exec_builtin`; run `expand(args, &mut self)`,
then push the result as a `ScriptAction::ExecLine(expanded)` (or call
`dispatch_line` directly if in the event loop context).

---

## MEDIUM — Test Corpus Expansion

### [T3] Community TF script corpus — Task #39
Run real scripts through the batch-mode harness; fix divergences found.

**Script sources:**
- `huntwj/tf-util` — foundational lib; blocked on G5–G7 above
- `huntwj/tf-wotmud` — WoT MUD; blocked on G8 (/eval)
- `huntwj/tf-diku`, `tf-sqlite`, `tf-mapper`
- `Sketch/tinyfugue-scripts`
- `DrDrifter/TinyFugue_BatMUD`

**Approach**: Clone each repo; attempt `/load` in batch mode (`-n`); collect
errors; add targeted compat-test cases; implement missing features.

---

### [H4] `/help` system output — Task #35
C TF uses a compiled index (`tf-help.idx`) mapping topic names to byte
offsets in a help document.  We embed the index but never query it.

**C source**: `command.c` `handle_help_command`, `makehelp.c`.

**Fix**: Parse the embedded `tf-help.idx`; on `/help topic`, binary-search
for the topic and extract the relevant section from the embedded help text.

---

## LOW — Polish and Parity

### [Q4] Macro priority tiebreak ordering — Task #36
When two triggers have equal priority, C TF fires the most-recently-defined
one first.  Our sort may break ties differently.

**Fix**: Add a definition-sequence counter to `Macro`; include it (reversed)
as a sort tiebreak in `MacroStore::find_triggers`.

---

### [X3] Per-world mfile not re-sourced on `/fg` — Task #36
C TF re-sources a world's `mfile` every time you foreground it when
`%sockmload` is set.  Useful for world-specific keybinds that refresh on
switch.

**C source**: `socket.c:1229` — `if (sockmload) wload(sock->world)`.

**Fix**: In the `/fg` handler, after switching active world, check
`%sockmload`; if set, re-source `world.mfile`.

---

### [L1] Startup message order doesn't match C TF — Task #27
Version banner, locale messages, and "Loading commands from …" appear in a
slightly different order than C TF.  Cosmetic only.

**Fix**: Compare `main.c` startup sequence step-by-step against `main.rs`
and reorder `push_line`/`println!` calls.

---

### [L2] Terminal exit behaviour — Task #36
C TF clears the screen on exit and places the shell prompt at the top.
Current Rust behaviour may actually be preferable; deferred.

---

## VERY LOW — Performance

### [Perf] Tree-walking interpreter vs bytecode VM
The C interpreter compiles macro bodies to bytecode; we tree-walk the AST.
Profile before optimising — I/O dominates most MUD workloads.

**Investigation**: Profile a trigger-heavy session.  If significant:
consider caching compiled ASTs on `Macro`, then bytecode compilation.

---

## Already Resolved

### Scripting language
- [x] `@@var` indirect expansion — `expand.rs` handles `@@name`
- [x] `%(expr)` inline expression form — handled in `expand.rs`
- [x] `/shift` — removes `args[0]` from current frame
- [x] `/result` — stores last `/test` result in `interp.last_result`
- [x] `isset(name)` — alias for `isvar` in `call_fn`
- [x] `.` string concatenation operator — `BinOp::Concat` in `expr.rs`
- [x] `{body}` inline form for `/for` and `/while`
- [x] `%;` separator inside `{...}` blocks — `split_by_separator` tracks brace depth
- [x] Inline `/else /cmd` body — `parse_if` includes else-line body
- [x] `$[%var-expr]` — expression content is pre-expanded before `eval_str`
- [x] `kbwordleft([n])` / `kbwordright([n])` — `call_fn` using word-boundary model
- [x] `strcmpattr(s1, s2)` — strips `@{...}` markup then compares
- [x] `read()` — reads one line from stdin

### Commands and built-ins
- [x] `send(text[, world[, flags]])` — queued via `ScriptAction::SendToWorld`
- [x] `/gag [pattern]` — no-arg sets `%gag=1`; with pattern creates gag trigger
- [x] `/hilite [pattern]` — mirrors `/gag` with hilite attr
- [x] `/relimit` / `/unlimit` — stubs setting `%more` 1/0
- [x] `/core` — stub printing "not implemented in Rust build"
- [x] `/edit` — opens `$EDITOR` on temp file, re-inserts result
- [x] `/liststreams` — lists `tf_files` by fd and mode
- [x] `/trigger -h<HOOK>` — fires hook directly at runtime
- [x] `/connect host port` — two-arg form creates a WORLD_TEMP world
- [x] `/fg` flags — `-c<n>` (index), `-n`/`-q`; world cycling
- [x] `/recall` flags — `-n N`, `-b` (reverse), `-w world`, bare pattern; reads screen scrollback
- [x] `/save` — writes `/addworld` then `/def` lines
- [x] `morescroll(n)` — queues `ScriptAction::Scroll(n)`
- [x] `morepaused()` — returns `_morepaused` global
- [x] `nmail()` / `nread()` — stubbed returning 0
- [x] `prompt(text)` — sets input prompt via `ScriptAction::SetPrompt`
- [x] `send()` `"h"` flag — fires SEND hook after sending
- [x] `world_info()` all 11 fields
- [x] `nactive(worldname)` — per-world active check

### Startup and connection
- [x] `%visual` / `%interactive` — set from `isatty()` after config load
- [x] `Hook::World` — fired on connect, switch, and `-n` (no world)
- [x] `-l` / `-q` flags — `no_autologin` / `quiet_login` threaded to connect
- [x] Early `puts()` banner — `println!()` calls before `EventLoop::new()`
- [x] `/version` — includes mods string and platform
- [x] `LANG`, `LC_ALL`, `LC_CTYPE`, `LC_TIME`, `TZ`, `MAIL`, `TERM`, `TFPATH` — read from env
- [x] Variable defaults (`%quiet`, `%gag`, `%hilite`, `%scroll`, `%wrap`, `%login`,
  `%sub`, `%more`) — set before stdlib load
- [x] `/dc` — fires H_DISCONNECT, removes handle, switches active world
- [x] `nactive` — counts background worlds with unread output (not open handle count)

### Triggers and rendering
- [x] `%gag` — checked in render pipeline; gagged lines suppressed
- [x] Trigger attr merging — all matching trigger attrs ORed together
- [x] Trigger `-c<n>` self-destruct count — `TriggerAction.shots`, auto-remove at 0
- [x] `/more` paging — `more_threshold` synced from `%more` × winlines; any key unpauses

### Testing infrastructure
- [x] Non-interactive / batch mode (`run_batch()` — stdin line-by-line, output to stdout)
- [x] C-vs-Rust compat test harness — 20 test cases in `tf-rs/tests/compat_tests.rs`

### Embedded library
- [x] All 47 `lib/tf/*.tf` files embedded via `include_bytes!()` in `embedded.rs`
- [x] `--install-libs [<dir>]` — extracts embedded files to OS data dir
- [x] `resolve_libdir` fallback chain: `-L` → `$TFLIBDIR` → dev path → OS data dir → embedded
