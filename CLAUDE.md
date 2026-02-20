# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build System

TinyFugue uses autoconf. `src/Makefile` is auto-generated — do not edit it directly. Edit `src/vars.mak`, `unix/unix.mak`, or `src/rules.mak` instead.

```bash
# First-time or after configure.ac changes
./configure [options]

# Build
make

# Install (installs tf binary and lib files)
make install

# Clean build artifacts
make clean
```

Common `./configure` options:
- `--enable-python` — embed Python interpreter
- `--enable-lua` — embed Lua scripting
- `--disable-widechar` — disable ICU/UTF-8 wide character support
- `--enable-atcp`, `--enable-gmcp`, `--enable-option102` — telnet protocol extensions
- `--disable-ssl` — disable OpenSSL support

The CI matrix (`build.yml`) tests combinations of `--enable-lua`, `--enable-python`, and `--disable-widechar` against multiple Python versions.

There are no automated tests; verification is done by building and running tf manually.

## Required System Libraries

- `libpcre2-8` — regex (PCRE2)
- `libssl`, `libcrypto` — OpenSSL
- `libtermcap` or `ncurses`
- `libicui18n`, `libicuuc`, `libicudata` — ICU (for widechar/UTF-8)
- `libz` — zlib
- Lua: `liblua5.4-dev` (optional)
- On Ubuntu: `sudo apt-get install lua5.4 liblua5.4-dev libpcre2-dev libicu-dev`

> **Lua note:** `lua.c` uses Lua 5.3+ APIs. `/usr/bin/lua` may point to Lua 5.1, causing a link failure. Always specify `LUA=/usr/bin/lua5.4` when configuring with `--enable-lua`:
> ```bash
> LUA=/usr/bin/lua5.4 ./configure --enable-lua
> ```

## Architecture

TinyFugue is a single-process, event-driven C MUD client built around a `select()`-based main loop in `src/socket.c`.

### Key subsystems and their source files

| Area | Files |
|------|-------|
| Entry point & init | `main.c` |
| Network/MUD connections | `socket.c`, `world.c` |
| Terminal I/O | `tty.c`, `output.c`, `tfio.c` |
| Keyboard input & key bindings | `keyboard.c`, `keylist.h` |
| Macro/trigger engine | `macro.c`, `expand.c` |
| TF scripting language | `expr.c`, `parse.h`, `opcodes.h`, `command.c` |
| Variable system | `variable.c`, `varlist.h` |
| Pattern matching | `pattern.c` (wraps PCRE2) |
| History/recall | `history.c` |
| Signal handling | `signals.c` |
| Text attributes/colors | `attr.c`, `tf.h` |
| Python embedding | `tfpython.c` |
| Lua embedding | `lua.c` |
| Help index builder | `makehelp.c` (standalone tool) |

### Code-generation headers (X-macro pattern)

Several `.h` files are not ordinary headers — they are included multiple times with different macro definitions to generate both enum constants and data tables from a single source of truth:

- `enumlist.h` — defines enumerated option values (e.g., `BAMF_OFF`, `META_ON`) via `bicode(enum_const, string_value)`
- `hooklist.h` — defines event hook IDs (e.g., `H_CONNECT`, `H_DISCONNECT`) via `gencode(ID, flags)`
- `cmdlist.h` — defines built-in `/commands` via `defcmd(name, func, reserved)`
- `funclist.h` — defines built-in expression functions
- `varlist.h` — defines global TF variables

When adding a new hook, command, or variable, update the corresponding list header rather than scattered declarations.

### String types

TF has its own string abstraction (`dstring.h`): `String` (mutable) and `conString` (immutable/literal). These are distinct from `char *`. The `malloc.h` header wraps allocation with optional debug tracing.

## Rust Rewrite (`tf-rs/`)

The root `Cargo.toml` is a workspace with `tf-rs/` as its only member.

```bash
cargo build          # build
cargo test           # run tests
cargo clippy         # lint — must be clean before committing
```

**All Rust commits must have zero `cargo clippy` warnings.** Run clippy and fix any warnings before creating a changeset.

---

### Configuration flow

`configure.ac` → autoconf → `src/Makefile` (assembled by concatenating `unix/vars.mak`, `src/vars.mak`, `unix/unix.mak`, `src/rules.mak`). Feature flags become `#define`s in `src/tfconfig.h`. Feature-dependent code is guarded by `#ifdef HAVE_SSL`, `#ifdef WIDECHAR`, `#ifdef ENABLE_GMCP`, etc.
