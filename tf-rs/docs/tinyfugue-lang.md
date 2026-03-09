# TinyFugue Scripting Language Reference

This document describes the TF scripting language as implemented in the Rust
rewrite (`tf-rs`).  It covers script structure, text expansion, expressions,
statements, built-in commands and functions, variables, macros, hooks, and
pattern matching.

---

## Table of Contents

1. [Script Structure](#1-script-structure)
2. [Text Expansion](#2-text-expansion)
3. [Expression Language](#3-expression-language)
4. [Variables and Scope](#4-variables-and-scope)
5. [Statements and Control Flow](#5-statements-and-control-flow)
6. [Macros, Triggers and Hooks](#6-macros-triggers-and-hooks)
7. [Pattern Matching](#7-pattern-matching)
8. [Built-in Commands](#8-built-in-commands)
9. [Built-in Functions](#9-built-in-functions)
10. [Hooks Reference](#10-hooks-reference)
11. [File I/O](#11-file-io)

---

## 1. Script Structure

### Source files

TF scripts are plain text files loaded with `/load file` or `/require file`.
Lines starting with `;` or `#` are comments and are ignored.

### Line continuation

A line ending with `\` is joined with the following line (the backslash and
newline are removed).  Leading whitespace on the continuation line is stripped.

```tf
/def mymacro = \
    /echo hello%;\
    /echo world
```

### Statement separator

Within a single logical line, `%;` separates statements:

```tf
/echo hello%; /echo world
```

### Bare lines

A line that does not start with `/` is sent verbatim to the current world
connection.

---

## 2. Text Expansion

Most command arguments go through **text expansion** before execution.
Expansion substitutes variable references, positional parameters, and
expression results into the text.

### Variable references

| Sequence            | Result                                              |
|---------------------|-----------------------------------------------------|
| `%name`             | Value of variable `name`                            |
| `%{name}`           | Value of variable `name` (brace form)               |
| `${name}`           | Value of variable `name` (dollar-brace form)        |
| `%{name-default}`   | Value of `name`, or `default` if unset/empty        |

### Positional parameters

Within a macro body, positional parameters refer to the arguments the macro
was called with.

| Sequence      | Result                                                      |
|---------------|-------------------------------------------------------------|
| `{1}`, `%1`   | First positional parameter                                  |
| `{2}`, `%2`   | Second positional parameter (etc.)                          |
| `{#}`, `%#`   | Number of positional parameters                             |
| `{*}`, `%*`   | All parameters joined with spaces                           |
| `{L}`, `%L`   | Last positional parameter                                   |
| `{-1}`, `%-1` | All parameters from index 1 onward (i.e., all but first)   |
| `{-L}`, `%-L` | All parameters except the last                              |
| `{N-default}` | Param N, or `default` if not supplied                       |
| `{L-default}` | Last param, or `default` if no params                       |
| `{*-default}` | All params joined, or `default` if no params                |
| `{P}`, `%P`   | Name of the currently executing macro/command               |
| `%R`          | A random positional parameter                               |

### Expression substitution

| Sequence    | Result                                                          |
|-------------|-----------------------------------------------------------------|
| `$[expr]`   | Evaluate `expr` as a TF expression; substitute the result       |
| `%(expr)`   | Same as `$[expr]` (alternate form)                              |
| `$(cmd)`    | Execute `cmd`, capture its `/echo` output; substitute that text |

### Indirect expansion

| Sequence    | Result                                                          |
|-------------|-----------------------------------------------------------------|
| `@@name`    | Value of `%name`, then value of the variable with that name     |

### Escaping

| Sequence | Result          |
|----------|-----------------|
| `$$`     | Literal `$`     |
| `%;`     | Statement separator (not a literal semicolon in most contexts) |

### Expression-context expansion (`/test`, `/result`, `/return`)

In `/test`, `/result`, `Stmt::Return`, and `Stmt::Expr` contexts, positional
parameters (`{N}`, `%N`, etc.) are automatically quoted as string literals
before the result is evaluated as an expression.  This means:

- `{1}` with param `"mycolor"` → the string `"mycolor"`, **not** a variable
  reference to `%mycolor`.
- `%{name}` still expands to the **raw value** of `%name`, and that value is
  then evaluated as an expression (C TF double-dereference semantics).
- `util_event_%{_event}` with `_event="click"` → `util_event_click` (dynamic
  variable name construction via text concatenation).

---

## 3. Expression Language

Expressions appear inside `$[...]`, `%(...)`, `/test`, `/result`, and
`/return`.

### Types

TF expressions are dynamically typed with three value types:

| Type    | Examples               | Notes                                     |
|---------|------------------------|-------------------------------------------|
| Integer | `0`, `42`, `-7`        | 64-bit signed                             |
| Float   | `3.14`, `1e-3`         | 64-bit double                             |
| String  | `"hello"`, `'world'`   | UTF-8; arithmetic coerces to number       |

Hex literals (`0x1F`) are supported.  Strings are coerced to numbers when used
in arithmetic contexts.  `0`, `""`, and `0.0` are falsy; everything else is
truthy.

### Operators (highest to lowest precedence)

| Precedence | Operators                    | Description                         |
|------------|------------------------------|-------------------------------------|
| Unary      | `-x`, `!x`, `~x`            | Negate, logical not, bitwise not    |
| Multiply   | `*`, `/`, `%`                | Multiply, divide, remainder         |
| Add        | `+`, `-`, `.`                | Add, subtract, concatenate strings  |
| Shift      | `<<`, `>>`                   | Bitwise shift                       |
| Bitwise    | `&`, `^`, `\|`               | AND, XOR, OR                        |
| Compare    | `==`, `!=`, `<`, `<=`, `>`, `>=` | Numeric/string comparison       |
| Pattern    | `=~`, `!~`                   | Glob match / no-match               |
| Pattern    | `=/`, `!/`                   | Regex match / no-match              |
| Logical    | `&&`, `\|\|`                 | Short-circuit AND / OR              |
| Ternary    | `cond ? then : else`         | Conditional expression              |
| Elvis      | `expr ?: default`            | Value if truthy, else default       |
| Assign     | `=`, `+=`, `-=`, `*=`, `/=`, `%=` | Local assignment              |
| Global assign | `name :=`               | Assign to global variable `name`    |
| Indirect assign | `%name :=`, `%{name} :=` | Assign to global variable named by `%name` |
| Comma      | `,`                          | Sequence; returns last value        |

### Assignment

```tf
x = 42          ; local variable (visible only in current macro)
y := "hello"    ; global variable
%ptr := "val"   ; global variable whose name is the value of %ptr
```

### Variable references in expressions

| Form       | Meaning                                                         |
|------------|-----------------------------------------------------------------|
| `name`     | Value of variable `name` (single lookup)                        |
| `%name`    | Same as `name` (single lookup)                                  |
| `%{name}`  | Evaluate value of `%name` as an expression (n-level deref)     |
| `{N}`      | Nth positional parameter (direct value, not a variable lookup)  |
| `{#}`      | Count of positional parameters                                  |
| `{*}`      | All parameters joined with space                                |

### Function calls

```tf
$[strlen("hello")]        ; → 5
$[strcat("a", "b", "c")] ; → "abc"
$[rand(10)]               ; → random integer 0–9
```

### Indirect function call

```tf
/set _fn=myMacro
/test %{_fn}("arg1", "arg2")   ; calls myMacro("arg1", "arg2")
```

---

## 4. Variables and Scope

### Local variables (`/let`, `=`)

Local variables are scoped to the current macro call frame.  They shadow
globals of the same name.

```tf
/let _tmp=hello
/test x = 42
```

### Global variables (`/set`, `:=`)

Global variables persist across macro calls.

```tf
/set myvar=hello
/test myvar := "hello"
```

### Unsetting

```tf
/unset myvar
```

### Special built-in variables

| Variable   | Meaning                                              |
|------------|------------------------------------------------------|
| `%P0`–`%P9` | Regex capture groups from last `regmatch()` call   |
| `%PL`      | Text before last regex match                         |
| `%PR`      | Text after last regex match                          |
| `%R`       | Random positional parameter                          |

### System variables (read/write)

TF maintains many named system variables controlling client behaviour
(e.g., `%visual`, `%interactive`, `%kbnum`, `%insert`).  Use `/set` to
change them and `%varname` to read them.

---

## 5. Statements and Control Flow

### Conditionals

```tf
/if (expr) \
    /echo true%; \
/else \
    /echo false%; \
/endif
```

Inline form (single statement each branch):

```tf
/if (x > 0) /echo positive%; /else /echo nonpositive%; /endif
```

`/elseif (expr)` is also supported.

### While loop

```tf
/let i=0
/while (i < 5) \
    /echo %i%; \
    /let i=$[i+1]%; \
/done
```

### For loop

```tf
/for i 1 10 \
    /echo %i%; \
/done
```

Iterates `i` from `start` to `end` inclusive.

### Break / Return

```tf
/while (1) \
    /break%;\        ; exit loop
/done

/def double = /return $[{1} * 2]
$[double(7)]         ; → 14
```

`/return [expr]` exits the current macro (or function call) with a value.
`/test` and `/result` evaluate an expression and return its value.

### Let / Set / Unset

```tf
/let _local=value        ; local variable
/set global=value        ; global variable
/unset varname           ; delete a variable
/shift                   ; drop first positional param; renumber remaining
```

### Echo

```tf
/echo text               ; print to output (with newline)
/echo -n text            ; print without newline
```

### Eval

```tf
/eval /echo hello        ; parse and execute a string as a TF command
```

---

## 6. Macros, Triggers and Hooks

### Defining macros

```tf
/def name = body
/def -i name = body      ; case-insensitive
/def -p5 name = body     ; priority 5 (higher = checked first)
/def -c3 name = body     ; auto-delete after 3 firings
/def -q name = body      ; quiet (don't print "defined" message)
```

**Body** is any TF script text.  Within the body, `{1}` … `{9}`, `{*}`, etc.
are the positional parameters passed when the macro is called.

### Calling macros

```tf
/mymacro arg1 arg2       ; command form — args split on whitespace
$[mymacro("arg1", "arg2")]  ; expression form — args are expressions
```

### Triggers

Triggers fire when incoming text from a world matches a pattern.

```tf
/def -t"pattern" [-wworld] [-p5] [-c1] name = body
```

| Flag        | Meaning                                              |
|-------------|------------------------------------------------------|
| `-t"pat"`   | Trigger pattern (glob by default)                    |
| `-w world`  | Only fire for named world                            |
| `-p N`      | Priority                                             |
| `-c N`      | Fire at most N times then self-destruct              |
| `-P`        | Pattern is PCRE2 regex                               |
| `-F`        | Pattern is plain substring (fast)                    |
| `-i`        | Case-insensitive match                               |
| `-ag`       | Apply attribute: gag (suppress) matched line         |
| `-ah`       | Apply attribute: highlight matched line              |

Within a trigger body, `%{1}` etc. are regex capture groups if `-P` was used.

### Hooks

Hooks fire on system events.

```tf
/def -hCONNECT name = body     ; fires when connected to a world
/def -hDISCONNECT name = body  ; fires when disconnected
```

See [§10 Hooks Reference](#10-hooks-reference) for the full list.

### Bindings

```tf
/bind ^A = /echo ctrl-A pressed
/bind KEY_F1 = /echo F1 pressed
```

### Undefining

```tf
/undef name        ; remove macro by name
/unbind sequence   ; remove key binding
/purge pattern     ; remove all macros matching name pattern
```

---

## 7. Pattern Matching

TF supports four pattern types:

| Type      | Flag | Syntax                         | Notes                              |
|-----------|------|--------------------------------|------------------------------------|
| Glob      | (default) | `*`, `?`, `[abc]`        | Shell-style wildcards              |
| Regex     | `-P` | PCRE2 regular expression       | Full PCRE2 syntax                  |
| Simple    | `-s` | Literal substring or wildcard  | `*` matches anything               |
| Substring | `-F` | Literal text                   | Fast fixed-string search           |

In expressions:

```tf
$["hello world" =~ "*world"]    ; glob match → 1
$["hello world" =/ "wor.d"]     ; regex match → 1
$["hello world" !~ "*xyz"]      ; glob no-match → 1
```

`regmatch(pattern, text)` runs a PCRE2 match and populates `%P0`–`%Pn`,
`%PL`, `%PR`.

---

## 8. Built-in Commands

### World / connection

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/addworld name host port [type]`| Define a world                           |
| `/connect [name]`                | Connect to world (or first defined)      |
| `/disconnect [name]` / `/dc`     | Disconnect from world                    |
| `/fg [name]`                     | Switch active (foreground) world         |
| `/world [name]`                  | Alias for `/fg`                          |
| `/listworlds` / `/worlds`        | List defined worlds                      |
| `/saveworld [file]`              | Save world definitions                   |
| `/unworld name`                  | Remove a world definition                |

### Macro management

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/def [-flags] name = body`      | Define a macro/trigger/hook/binding      |
| `/undef name`                    | Remove a macro                           |
| `/purge [pattern]`               | Remove all matching macros               |
| `/list [-flags] [pattern]`       | List macros                              |
| `/listvar [-flags] [pattern]`    | List variables                           |
| `/trigger flags`                 | Define a trigger (alias for `/def -t`)   |
| `/hook flags`                    | Define a hook (alias for `/def -h`)      |
| `/bind sequence = body`          | Bind a key                               |
| `/unbind sequence`               | Remove a key binding                     |

### Script execution

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/load [-q] file`                | Load and execute a script file           |
| `/require file`                  | Load once (noop if already loaded)       |
| `/eval expr`                     | Evaluate TF script text                  |
| `/test expr`                     | Evaluate expression; return result       |
| `/result expr`                   | Alias for `/test` (expression context)   |
| `/shift`                         | Shift positional params left             |

### Variables

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/set name=value`                | Set global variable                      |
| `/let name=value`                | Set local variable                       |
| `/unset name`                    | Unset a variable                         |
| `/setenv NAME=value`             | Set environment variable                 |
| `/export name`                   | Export TF variable to environment        |

### Output

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/echo [-n] text`                | Print text to output                     |
| `/input text`                    | Set text in input buffer                 |
| `/status [-fields] [values]`     | Configure/update status line             |
| `/recall [N]`                    | Recall history lines                     |
| `/log file`                      | Start logging to file                    |
| `/nolog`                         | Stop logging                             |
| `/save [file]`                   | Save macros to file                      |
| `/beep`                          | Ring terminal bell                       |

### Process control

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/repeat [-i ms] [-t N] body`    | Repeat `body` every N ms (at most T times)|
| `/quote ['file \| !cmd] [-i ms]` | Send file/command output to world        |
| `/ps`                            | List background processes                |
| `/kill pid`                      | Stop a background process                |
| `/sh command`                    | Run shell command                        |
| `/edit [file]`                   | Open `$EDITOR` on a temp file            |

### Appearance

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/gag [-flags] [pattern]`        | Gag (suppress) matching lines            |
| `/hilite [-flags] [pattern]`     | Highlight matching lines                 |
| `/relimit N`                     | Set scrollback limit                     |
| `/unlimit`                       | Remove scrollback limit                  |
| `/histsize N`                    | Set history buffer size                  |
| `/visual [on\|off]`              | Enable/disable visual (full-screen) mode |
| `/redraw`                        | Redraw the screen                        |

### Miscellaneous

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/version`                       | Print version string                     |
| `/features`                      | Print compiled-in features               |
| `/restrict [shell\|world]`       | Restrict client capabilities             |
| `/suspend`                       | Suspend process (SIGSTOP)                |
| `/quit` / `/exit`                | Exit tf                                  |
| `/help [topic]`                  | Show help                                |
| `/dokey keyname`                 | Simulate a key action                    |
| `/lcd [dir]`                     | Change working directory                 |

### Lua / Python embedding

| Command                          | Description                              |
|----------------------------------|------------------------------------------|
| `/loadlua file`                  | Load a Lua script                        |
| `/calllua fn [args]`             | Call a Lua function                      |
| `/loadpython file`               | Load a Python script                     |
| `/callpython fn [args]`          | Call a Python function                   |
| `/python code`                   | Execute Python code inline               |

---

## 9. Built-in Functions

Built-in functions are called from expression context: `$[fn(args)]`.

### String functions

| Function                        | Returns                                          |
|---------------------------------|--------------------------------------------------|
| `strlen(s)`                     | Character count of `s`                           |
| `strcat(s, ...)`                | Concatenate all arguments                        |
| `substr(s, pos [, len])`        | Substring starting at `pos` (0-based)            |
| `strcmp(a, b)`                  | -1 / 0 / 1 (lexicographic)                      |
| `strcmpattr(a, b)`              | `strcmp` after stripping display-attribute markup|
| `strncmp(a, b, n)`              | Compare first `n` characters                     |
| `strchr(s, ch [, offset])`      | First index of char `ch` in `s` (or -1)          |
| `strrchr(s, ch [, offset])`     | Last index of char `ch` in `s` (or -1)           |
| `strstr(haystack, needle [, off])` | First index of `needle` in `haystack` (or -1) |
| `replace(old, new, s)`          | Replace all occurrences of `old` with `new` in `s`|
| `toupper(s)`                    | Uppercase                                        |
| `tolower(s)`                    | Lowercase                                        |
| `strrep(s, n)`                  | Repeat `s` `n` times                            |
| `pad(s, n [, ch])`              | Pad/truncate `s` to width `n` (right-pad with `ch`)|
| `ascii(s)`                      | ASCII/Unicode codepoint of first char            |
| `char(n)`                       | Character with codepoint `n`                     |
| `regmatch(pattern, text)`       | 1 if PCRE2 pattern matches; sets `%P0`–`%Pn`, `%PL`, `%PR` |
| `textencode(s)`                 | URL-percent-encode `s`                           |
| `textdecode(s)`                 | URL-percent-decode `s`                           |
| `strip_attr(s)`                 | Strip `@{...}` display-attribute markup          |
| `decode_ansi(s)`                | Convert ANSI escape sequences to `@{...}` markup |
| `encode_ansi(s)`                | Convert `@{...}` markup to ANSI escape sequences |
| `decode_attr(s)`                | Decode attribute markup                          |
| `encode_attr(s)`                | Encode attribute markup                          |

### Math functions

| Function          | Returns                                     |
|-------------------|---------------------------------------------|
| `abs(n)`          | Absolute value                              |
| `mod(a, b)`       | `a mod b` (always non-negative)             |
| `rand(n)`         | Random integer in `[0, n)`                  |
| `trunc(x)`        | Truncate to integer                         |
| `sqrt(x)`         | Square root                                 |
| `pow(x, y)`       | `x` to the power of `y`                    |
| `sin(x)` / `cos(x)` / `tan(x)` | Trig (radians)            |
| `asin(x)` / `acos(x)` / `atan(x [, y])` | Inverse trig          |
| `exp(x)` / `ln(x)` | Exponential / natural log                |

### Time functions

| Function              | Returns                                       |
|-----------------------|-----------------------------------------------|
| `time()`              | Current Unix timestamp (integer seconds)      |
| `ftime(fmt [, t])`    | Format timestamp `t` (or now) with `strftime` |
| `mktime(str)`         | Parse time string to Unix timestamp           |

### System / introspection functions

| Function                | Returns                                           |
|-------------------------|---------------------------------------------------|
| `isvar(name)`           | 1 if variable `name` is set, else 0              |
| `isset(name)`           | Alias for `isvar`                                 |
| `ismacro(name)`         | 1 if macro `name` is defined, else 0             |
| `whatis(x)`             | String describing type of `x` (`"int"`, `"float"`, `"string"`) |
| `systype()`             | OS name string                                   |
| `getpid()`              | Process ID                                       |
| `gethostname()`         | Hostname                                         |
| `features()`            | Compiled-in feature flags string                 |
| `keycode(key)`          | Terminal keycode string for key name             |
| `isatty()`              | 1 if stdin is a terminal                         |

### World / connection functions

| Function              | Returns                                         |
|-----------------------|-------------------------------------------------|
| `worldname()`         | Name of active world                            |
| `nworlds()`           | Number of defined worlds                        |
| `nactive([world])`    | Worlds with unread output (or 1/0 for named)    |
| `fg_world()`          | Name of foreground world                        |
| `is_open(world)`      | 1 if world connection is open                   |
| `is_connected(world)` | 1 if world is fully connected                   |
| `world_info(world, field)` | Field from world definition               |
| `idle([world])`       | Seconds since last input sent to world          |
| `sidle([world])`      | Seconds since last line received from world     |

### Display functions

| Function              | Returns                                         |
|-----------------------|-------------------------------------------------|
| `columns()`           | Terminal width in columns                       |
| `lines()` / `winlines()` | Terminal height in rows                    |
| `moresize()`          | Lines of output currently held in `/more` pause |
| `morescroll(n)`       | Scroll `n` lines; returns count scrolled        |
| `morepaused()`        | 1 if output is paused by `/more`                |
| `status_fields()`     | Number of status line fields                    |
| `status_width(n)`     | Width of status field `n`                       |
| `status_label(n)`     | Label of status field `n`                       |

### Input / keyboard functions

| Function          | Returns                                         |
|-------------------|-------------------------------------------------|
| `kblen()`         | Length of current input buffer                  |
| `kbpoint()`       | Cursor position in input buffer                 |
| `kbhead()`        | Text before cursor in input buffer              |
| `kbtail()`        | Text from cursor to end                         |
| `kbdel(n)`        | Delete `n` chars at cursor; returns deleted text|
| `read()`          | Read a line from stdin (blocks)                 |

### File functions

| Function                    | Returns / Effect                              |
|-----------------------------|-----------------------------------------------|
| `tfopen(path, mode)`        | Open file; returns handle (int) or -1         |
| `tfclose(handle)`           | Close file handle                             |
| `tfread(handle)`            | Read next line; returns string or "" at EOF   |
| `tfwrite(handle, text)`     | Write `text` to file                          |
| `tfflush(handle)`           | Flush file                                    |
| `tfreadable(handle)`        | 1 if data is available to read                |
| `filename(path)`            | Basename of `path`                            |
| `dirname(path)`             | Directory part of `path`                      |

Modes for `tfopen`: `"r"` (read), `"w"` (write/truncate), `"a"` (append).

### Substitution / formatting

| Function                   | Returns                                        |
|----------------------------|------------------------------------------------|
| `echo(text [, world, raw])` | Display `text` (optionally to `world`)        |
| `substitute(text, attrs)`  | Replace current trigger match line with `text` |
| `getopts(optstring, ...)`  | Parse option flags from positional params      |
| `attrout(attr)`            | Apply attribute to next output (stub)          |
| `limit(n)`                 | Get/set scrollback line limit                  |

---

## 10. Hooks Reference

Hooks are macros defined with `-h HOOKNAME`.  They fire on system events.
Hook bodies receive event-specific positional parameters.

| Hook name    | Fires when                                           | `{1}` / `{2}` etc.                     |
|--------------|------------------------------------------------------|-----------------------------------------|
| `ACTIVITY`   | Background world receives text                       | World name                              |
| `BAMF`       | Auto-connected via BAMF redirect                     | World name                              |
| `BGTEXT`     | Text arrives in background world                     | World name                              |
| `BGTRIG`     | Trigger fires in background world                    | World name                              |
| `CONFAIL`    | Connection attempt fails                             | World name, error                       |
| `CONFLICT`   | Macro name conflict on redefinition                  | Macro name                              |
| `CONNECT`    | Successfully connected to a world                    | World name                              |
| `DISCONNECT` | Disconnected from a world                            | World name                              |
| `ICONFAIL`   | Initial connection fails                             | World name                              |
| `KILL`       | A process is killed                                  | Process ID                              |
| `LOAD`       | A script file is loaded                              | Filename                                |
| `LOADFAIL`   | A script file fails to load                          | Filename                                |
| `LOG`        | Logging starts or stops                              | Filename / `""`                         |
| `LOGIN`      | World sends a login prompt                           | World name                              |
| `MAIL`       | New mail detected                                    | Mail count                              |
| `MORE`       | Output paging is triggered                           | —                                       |
| `NOMACRO`    | Unknown command typed                                | Command name                            |
| `PENDING`    | Outgoing data is waiting                             | World name                              |
| `PREACTIVITY`| Before ACTIVITY fires                               | World name                              |
| `PROCESS`    | A repeat/quote process fires                         | Process ID                              |
| `PROMPT`     | A prompt is received from world                      | World name                              |
| `PROXY`      | Proxy connection established                         | World name                              |
| `REDEF`      | A macro is redefined                                 | Macro name                              |
| `RESIZE`     | Terminal is resized                                  | Cols, rows                              |
| `SEND`       | Text is sent to a world                              | Text sent, world name                   |
| `SHADOW`     | A macro is shadowed by a new definition              | Macro name                              |
| `SHELL`      | Shell command completes                              | Exit code                               |
| `SIGHUP`     | Process receives SIGHUP                              | —                                       |
| `SIGTERM`    | Process receives SIGTERM                             | —                                       |
| `SIGUSR1`    | Process receives SIGUSR1                             | —                                       |
| `SIGUSR2`    | Process receives SIGUSR2                             | —                                       |
| `WORLD`      | Active world changes                                 | New world name, old world name          |
| `ATCP`       | ATCP telnet sub-negotiation received                 | Package name, value                     |
| `GMCP`       | GMCP telnet sub-negotiation received                 | Package name, value                     |
| `OPTION102`  | Telnet option 102 sub-negotiation received           | Data                                    |

### Hook example

```tf
/def -hCONNECT on_connect = \
    /echo Connected to %{1}%;\
    /send look
```

---

## 11. File I/O

TF provides a simple handle-based file API:

```tf
/let _h=$[tfopen("/tmp/data.txt", "r")]
/if (_h != -1) \
    /while (tfreadable(_h)) \
        /let _line=$[tfread(_h)]%;\
        /echo %_line%;\
    /done%;\
    /test tfclose(_h)%;\
/else \
    /echo Could not open file%;\
/endif
```

Writing:

```tf
/let _h=$[tfopen("/tmp/out.txt", "w")]
/test tfwrite(_h, "line one")
/test tfwrite(_h, "line two")
/test tfclose(_h)
```

The `tfread` function returns the next line **without** the trailing newline.
At end-of-file it returns `""`.

---

## Appendix: Differences from C TF

The Rust implementation closely follows C TF's behaviour but has some known
differences:

- **Positional params in `/test`/`/result`/`/return`**: `{N}` is quoted as a
  string literal before expression evaluation, so it never accidentally resolves
  as a variable name. C TF's expand-then-eval can cause `{N}` to be re-looked-up
  if its value happens to be a variable name.
- **`%{name}` in expressions**: Both implementations do double-dereference
  (expand `%name` to its value, then evaluate that as an expression), but the
  Rust implementation also supports `%{name}(args)` as an indirect function call.
- **`:=` global assignment**: Fully supported; `%name :=` and `%{name} :=` do
  single-deref indirect assignment.
- **MCP**: Not implemented (not in C TF either).
- **Visual/TUI mode**: Partial; raw mode input and crossterm rendering are
  implemented but some edge cases may differ.
