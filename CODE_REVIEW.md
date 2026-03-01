# tf-rs Code Review Findings

Issues identified by automated review of all source files under `tf-rs/src/`.
Each item has a status (`[ ]` open, `[x]` fixed), severity, file reference, and fix description.

---

## High Severity

### H1 — Blocking I/O inside async event loop
**File:** `event_loop.rs` (multiple handlers)
**Status:** [x]

`QuoteFileSync`, `StartLog`, `SaveWorlds`, `ShellInteractive`, `EditInput`, and `SaveMacros`
handlers all use `std::fs`/`std::process::Command` (blocking) inside async handler code.
This stalls the entire tokio executor — all network I/O freezes while waiting for a file
write or an external editor to exit.

**Fix:** Replace `std::fs::File`, `std::fs::OpenOptions`, `std::io::BufReader` with
`tokio::fs` equivalents. Replace `std::process::Command::new().status()` with
`tokio::process::Command::new().spawn()?.wait().await`.

---

### H2 — Predictable temp file name (symlink attack)
**File:** `event_loop.rs` ~line 1027 (`EditInput` handler)
**Status:** [x]

`format!("tf_edit_{}.txt", std::process::id())` creates a predictable path in `/tmp/`.
Classic TOCTOU symlink attack: another process can create a symlink at that path before
TF opens it, causing TF to write the user's input buffer to an arbitrary file.

**Fix:** Use `tempfile::NamedTempFile` (move from `dev-dependencies` to `dependencies`
in `Cargo.toml`). The file is automatically deleted on drop.

---

### H3 — `unwrap()` panic in `connect_world_by_name`
**File:** `event_loop.rs` ~line 1297
**Status:** [x]

```rust
let host = w.host.as_deref().unwrap();  // panics if host is None
```

`is_connectable()` checks this a few lines above, but that's a non-local invariant.
Any future refactor that reorders the calls causes a panic.

**Fix:** Replace with an explicit error path:
```rust
let Some(host) = w.host.as_deref() else {
    let msg = format!("% World '{}' has no host", w.name);
    self.screen.push_line(LogicalLine::plain(&msg));
    self.need_refresh = true;
    return;
};
```

---

### H4 — `set_var` undefined behaviour in multi-threaded runtime
**File:** `script/interp.rs` (`/setenv` and `/export` handlers)
**Status:** [x]

`unsafe { std::env::set_var(...) }` is called on the tokio runtime thread. With the
default `#[tokio::main]` multi-threaded runtime, other worker threads may concurrently
read env vars via `std::env::var()`, which is a data race and POSIX UB.

**Fix (preferred):** Switch to a single-threaded runtime:
```rust
#[tokio::main(flavor = "current_thread")]
async fn main() { ... }
```
TF is inherently single-connection-at-a-time and has no CPU-parallel work; the
current-thread runtime is appropriate and eliminates the entire class of thread-safety
concerns around `set_var` and other non-`Send` state.

---

### H5 — `=/` operator is a stub (performs substring search, not regex)
**File:** `script/expr.rs` (`regex_match` function)
**Status:** [x]

```rust
fn regex_match(text: &str, pattern: &str) -> bool {
    text.contains(pattern)   // BUG: this is substring search, not regex
}
```

Any user trigger or expression using `=/ "pattern"` silently does the wrong thing.
The `pattern` module already wraps the `regex` crate.

**Fix:**
```rust
fn regex_match(text: &str, pattern: &str) -> bool {
    Pattern::new(pattern, MatchMode::Regexp)
        .map(|p| p.matches(text))
        .unwrap_or(false)
}
```

---

### H6 — Fixed PRNG seed makes probabilistic triggers deterministic
**File:** `macros.rs` ~line 556
**Status:** [x]

The xorshift64 PRNG is seeded with a compile-time constant. `/def -pN` (probability-based
macro selection) produces identical results across every session and every restart.

**Fix:** Seed from the OS at startup:
```rust
fn make_seed() -> u64 {
    let mut buf = [0u8; 8];
    // getrandom is a transitive dep; alternatively read /dev/urandom directly
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| { use std::io::Read; f.read_exact(&mut buf)?; Ok(()) })
        .ok();
    u64::from_ne_bytes(buf)
}
```

---

### H7 — Silent MCCP decompression failure feeds garbage to telnet parser
**File:** `net.rs` (`mccp_decompress` / `Protocol::process`)
**Status:** [x]

When `flate2` decompression fails, the code silently falls back to treating the compressed
bytes as raw telnet input. The telnet parser then processes garbage bytes, producing
corrupt output or swallowing data with no visible indication of what went wrong.

**Fix:** On decompression failure, log an error to the screen and disconnect the world
rather than silently continuing.

---

### H8 — Potential deadlock in `python.rs`
**File:** `python.rs` (`pytf_world` ~line 121)
**Status:** [x]

`pytf_world` holds the `STATE: Mutex<...>` lock while calling `.lock()` on
`active_world: Mutex<...>`. If any other code path acquires `active_world` first and
then tries to acquire `STATE`, a deadlock results.

**Fix:** Clone the `Arc` out of `STATE` and release the outer lock before acquiring the
inner one:
```rust
let state_arc = STATE.lock().unwrap().clone(); // STATE lock released here
state_arc.and_then(|s| s.active_world.lock().unwrap().clone())
```

---

### H9 — Macro body re-parsed on every invocation
**File:** `script/interp.rs` `invoke_macro` ~line 1401
**Status:** [ ]

`parse_script(body)` is called every time a macro fires. For a trigger that matches
every incoming server line (common in active MUD sessions), this is `O(body_len)` parse
work per line received. A future parse failure also silently breaks the macro.

**Fix:** Add a `parsed_body: Option<Vec<Stmt>>` field to `Macro`. Populate it lazily on
first invocation (or eagerly at `/def` time). Cache thereafter.

---

### H10 — `try_send` silently drops Lua/Python commands
**File:** `lua.rs` ~line 180, `python.rs` ~line 194
**Status:** [x]

`tx.try_send(command)` discards commands without error when the channel is full (capacity
64). A busy Lua/Python script silently loses TF commands with no indication of failure.

**Fix:** Return an error to the scripting layer when the channel is full, or use a
blocking `send` wrapped in `spawn_blocking` so backpressure is applied rather than data
lost.

---

## Medium Severity

### M1 — Glob worst-case exponential complexity
**File:** `script/expr.rs` and `pattern.rs` (`glob_match_inner`)
**Status:** [ ]

The `*` wildcard case recurses on every suffix — O(2ⁿ) for pathological patterns like
`*a*a*a*b`. Low real-world risk but a malformed trigger body could hang TF.

**Fix:** Convert to a DP (dynamic programming) or NFA-based algorithm, or add a
recursion depth limit that returns `false` when exceeded.

---

### M2 — Single-pattern `AhoCorasick` for `Substr` matches
**File:** `pattern.rs` (`Pattern::Substr` compilation)
**Status:** [x]

`AhoCorasick` is heap-allocated for single-pattern substring search. AC provides its
benefit when matching many patterns simultaneously. For a single pattern, `str::find`
(or `memchr::memmem`) is faster and uses no heap allocation.

**Fix:** For `MatchMode::Substr` with a single pattern, use `memchr::memmem::Finder`
(already available via `aho-corasick`'s transitive deps) or just `str::find`.

---

### M3 — `screen.rs` `trim_to_max` is O(n²)
**File:** `screen.rs` `trim_to_max` ~lines 274–283
**Status:** [x]

For each logical line dropped, `retain` is called on the entire `physlines` Vec.
Dropping 100 logical lines is 100 × O(physlines) work.

**Fix:** Find the first physline with the target logical index in one pass, drain
everything before it, then decrement all remaining `logical_idx` values by the number
of dropped logical lines in a single second pass.

---

### M4 — `arith_add` misleading comment in `value.rs`
**File:** `script/value.rs` `arith_add` ~lines 101–109
**Status:** [x]

The comment says `+` performs string concatenation when either operand is non-numeric,
but the implementation always promotes both operands to numbers. Non-numeric strings
become `0`.

**Fix:** Remove the misleading comment, or implement string concatenation if that was
the intent (verify against C TF behaviour: C TF `+` is always numeric).

---

### M5 — Unknown token produces `Token::Eof` instead of a diagnostic
**File:** `script/expr.rs` lexer `_` arm ~line 327
**Status:** [x]

Unrecognised input bytes fall through to `Token::Eof`, giving "unexpected EOF" parse
errors instead of "unexpected character 'X'".

**Fix:** Add a `Token::Unknown(char)` variant and emit it from the `_` arm; the parser
reports the actual bad character.

---

### M6 — `input.rs` `text()` allocates `String` on every keystroke
**File:** `input.rs` `LineEditor::text()` ~line 48
**Status:** [x]

`self.buf.iter().collect::<String>()` allocates a new `String` on every call.
`text()` is called from `sync_kb_globals`, `refresh_display`, and `render_input` —
all running on every keystroke.

**Fix:** Cache the `String` in `LineEditor` and mark it dirty on every mutation
(`insert_char`, `delete_char`, etc.). `text()` returns a `&str` into the cached value.

---

### M7 — `python.rs` `print()` silently discarded
**File:** `python.rs` `INIT_SRC` ~line 161
**Status:** [x]

`sys.stdout` is redirected to `_TfStream(None)` which drops all output. Python `print()`
calls are silently swallowed, making debugging impossible.

**Fix:** Route Python `stdout` through `tf.out()` the same way `stderr` is routed, or
at minimum redirect it to `sys.stderr` so output appears somewhere.

---

### M8 — `invoke_macro` unknown-command path is silent
**File:** `script/interp.rs` `exec_builtin` unknown-command fallthrough
**Status:** [x]

Unknown commands silently return `Ok(None)`. While this matches C TF behaviour, it
makes diagnosing typos in macro bodies nearly impossible.

**Fix:** In debug mode (or always), push a `"% Unknown command: /foo"` message to
output when a command is not recognised. Make it suppressable with a flag if needed.

---

### M9 — `lua.rs` / `python.rs` blocking file read in async context
**File:** `python.rs` `run_file` ~line 244
**Status:** [x]

`std::fs::read_to_string` is called to load a Python script file while on the tokio
executor thread, blocking all async I/O for the duration of the read.

**Fix:** Use `tokio::fs::read_to_string(...).await` (requires the call site to be
`async`, which it is via the event loop handler chain).

---

## Low Severity

### L1 — `main.rs`: `ver` binding declared twice
**File:** `main.rs` lines 11 and 135
**Status:** [x]

`let ver = env!("CARGO_PKG_VERSION")` is declared at the top of `main()` and again
before the in-UI banner. The second shadows the first with the identical value.

**Fix:** Remove the second `let ver = ...`; reference the first binding.

---

### L2 — `cli.rs`: `install_embedded_libs` takes `&PathBuf` not `&Path`
**File:** `cli.rs` `install_embedded_libs` signature
**Status:** [x]

`&PathBuf` prevents callers from passing `&Path` directly and triggers clippy `ptr_arg`.

**Fix:** Change parameter type to `dest: &std::path::Path`.

---

### L3 — `cli.rs`: `find_user_config` constructs `"/.tfrc"` when `$HOME` unset
**File:** `cli.rs` `find_user_config`
**Status:** [x]

`std::env::var("HOME").unwrap_or_default()` silently uses `""` if `HOME` is unset,
producing paths like `"/.tfrc"` (root's config directory).

**Fix:** Return `None` immediately if `HOME` is unset or empty:
```rust
let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
```

---

### L4 — `embedded.rs`: silent UTF-8 decode failure
**File:** `embedded.rs` `get_embedded` / `all_embedded`
**Status:** [x]

`std::str::from_utf8(f.content).ok()` silently skips files with invalid UTF-8, making
them invisible rather than surfacing corruption at startup.

**Fix:** Use `.expect("embedded lib file is always valid UTF-8")` to catch corruption
loudly at first use.

---

### L5 — `hook.rs`: `HookSet::ALL` sets unused bits 35–63
**File:** `hook.rs`
**Status:** [x]

`HookSet::ALL = u64::MAX` sets bits 35–63 which correspond to no valid hook variant.
This means `HookSet::ALL != (all 35 individual hooks OR'd together)`, which could
surprise code that builds a set by OR-ing hooks and compares it to `ALL`.

**Fix:** Define `ALL` as `HookSet((1u64 << Hook::COUNT) - 1)` where `COUNT` is the
number of hook variants.

---

### L6 — `attr.rs`: `Attr::NONE` vs `Attr::EMPTY` confusing
**File:** `attr.rs`
**Status:** [x]

Two distinct "empty" sentinel values exist: `EMPTY` (all bits zero) and `NONE` (bit 5
set). `attr.contains(Attr::NONE)` returns `false` for an `EMPTY` attr, which is
non-obvious.

**Fix:** Add prominent documentation explaining the semantic difference, or consolidate
into a single sentinel if the distinction is not needed.

---

### L7 — `macros.rs`: `&self.macros[&num]` panics without useful message
**File:** `macros.rs` `find_triggers` ~line 385
**Status:** [x]

The HashMap indexing operator panics with an unhelpful message if the invariant
("`trig_list` only contains valid `num` values") is ever violated.

**Fix:**
```rust
let mac = self.macros.get(&num).expect("trig_list contains only valid macro nums");
```

---

### L8 — `process.rs`: `-1` sentinel for "infinite runs"
**File:** `process.rs` `Proc` struct
**Status:** [x]

`runs_left: i32` uses `-1` as a sentinel for "run forever". This is not idiomatic Rust.

**Fix:** Change to `runs_left: Option<u32>` where `None` means infinite. Update all
match/comparison sites.

---

### L9 — `tfstr.rs`: `to_lowercase()` allocates on every `@{...}` parse
**File:** `tfstr.rs` `tf_color_index`
**Status:** [x]

`name.to_lowercase()` allocates a `String` on every call. This runs in the hot path
when parsing ANSI/attribute sequences from incoming server lines.

**Fix:** Use `name.eq_ignore_ascii_case(candidate)` comparisons instead.

---

### L10 — `world.rs`: `to_addworld` emits `-s host` as a single token
**File:** `world.rs` `to_addworld`
**Status:** [x]

`parts.push(format!("-s {h}"))` emits `-s hostname` as one token. This works for
human-readable output but is inconsistent with how other flag/value pairs are pushed
and could confuse any future re-parser of the output.

**Fix:** Push `-s` and `h` as separate elements.

---

### L11 — `pattern.rs`: `Clone` re-compiles regex and can panic
**File:** `pattern.rs` `Pattern::clone`
**Status:** [x]

`Pattern::clone` calls `Pattern::new(...).expect("pattern recompile failed")`. If the
regex library ever rejects a previously-accepted pattern, `clone` panics — violating
the expectation that `Clone` is infallible.

**Fix:** Use `Arc<CompiledRegex>` inside `Pattern` so `clone` is a reference-count
increment rather than a recompile.

---

### L12 — `pattern.rs`: `has_unescaped_upper` ignores bracket classes
**File:** `pattern.rs` `has_unescaped_upper`
**Status:** [x]

An uppercase letter inside a bracket class like `[A-Z]` is incorrectly detected as
"has uppercase", suppressing the case-insensitive flag for the entire pattern including
parts outside the class.

**Fix:** Track whether the scanner is inside `[...]` and skip uppercase detection within
bracket classes.

---

### L13 — `history.rs`: `recall` loop invariant should be documented
**File:** `history.rs` `recall` prefix-search ~lines 152–196
**Status:** [x]

The loop uses `pos == start` to detect "wrapped all the way around with no match", but
the logic is non-obvious and fragile. A comment explaining the invariant would help
future maintainers.

---

### L14 — `script/stmt.rs`: `is_body_cmd` does not cover command abbreviations
**File:** `script/stmt.rs` `is_body_cmd` ~lines 152–158
**Status:** [x]

Only checks full command names (`/def`, `/trigger`, etc.). If the interpreter accepts
abbreviated forms (e.g., `/d` for `/def`), `is_body_cmd` would miss them and incorrectly
split the body on `%;`.

**Fix:** Either document that abbreviations are not supported in the parser, or enumerate
all accepted abbreviations.

---

*Last updated: after backlog audit completion. See git log for per-fix changesets.*
