/// Compatibility tests: run TF script snippets through the Rust binary (batch
/// mode) and verify the output matches expected values.
///
/// When the environment variable `TF_C_BINARY` is set (or when the C binary is
/// found at the default location), the same script is also run through C TF and
/// the outputs are compared.  This catches regressions where Rust and C produce
/// different results.
///
/// Each test case is a `(&str script, &[&str] expected_lines)` pair.  The
/// script is piped to the binary via stdin with `-n -f` flags; output is
/// normalised before comparison (see `normalise_output`).

use std::io::Write;
use std::process::{Command, Stdio};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Path to the Rust `tf` binary built by this Cargo workspace.
fn rust_binary() -> std::path::PathBuf {
    // CARGO_BIN_EXE_tf is set by cargo test infrastructure.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_tf"))
}

/// Try to locate the C TF binary.  Returns `None` if not available.
fn c_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("TF_C_BINARY") {
        let pb = std::path::PathBuf::from(&p);
        if pb.exists() { return Some(pb); }
    }
    let default = std::path::PathBuf::from("/home/wil/bin/tf");
    if default.exists() { return Some(default); }
    None
}

/// Run a TF script through a binary and collect its stdout + stderr.
fn run_tf(binary: &std::path::Path, script: &str, is_c_binary: bool) -> String {
    let mut cmd = Command::new(binary);
    cmd.args(["-n", "-f"]);
    if is_c_binary {
        // Force non-visual mode for the C binary so it doesn't try TUI rendering.
        cmd.arg("-v");
        cmd.env("TERM", "dumb");
    }
    cmd.stdin(Stdio::piped())
       .stdout(Stdio::piped())
       .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to spawn tf binary");
    {
        let stdin = child.stdin.as_mut().expect("stdin not open");
        // Append /quit in case the script doesn't exit cleanly.
        let full = format!("{}\n/quit\n", script.trim_end());
        stdin.write_all(full.as_bytes()).expect("write to stdin");
    }
    let out = child.wait_with_output().expect("wait failed");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    combined
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC [ ... final-byte  (CSI sequences) or ESC char (two-char sequences)
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // Consume until a letter (the final byte).
                    for c2 in chars.by_ref() {
                        if c2.is_ascii_alphabetic() { break; }
                    }
                }
                Some(_) => { chars.next(); } // ESC + one char (e.g. ESC =)
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Normalise output for comparison:
/// 1. Strip ANSI codes.
/// 2. Trim each line.
/// 3. Drop known preamble / system lines.
/// 4. Drop empty lines.
fn normalise_output(raw: &str) -> Vec<String> {
    strip_ansi(raw)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .filter(|l| {
            !l.starts_with("TinyFugue")
            && !l.starts_with("Copyright")
            && !l.starts_with("Type `")
            // LC_* locale messages — both "% LC_" (C TF) and "LC_" (Rust) forms.
            && !l.starts_with("% LC_")
            && !l.starts_with("LC_")
            && !l.starts_with("% Loading commands")
            && !l.starts_with("% Rust rewrite")
            && !l.starts_with("% Built for")
            // C TF echoes input commands in TERM=dumb mode.
            && !l.starts_with('/')
            // C TF also prints "Ingwar …" / "Using PCRE …" build info.
            && !l.starts_with("Ingwar")
            && !l.starts_with("Using PCRE")
        })
        .collect()
}

/// Whether C-binary cross-comparison is enabled.
///
/// Set `TF_C_COMPARE=1` to enable.  Disabled by default because the C TF
/// binary uses curses even in non-visual mode, making its stdout hard to
/// normalise reliably.  When enabled, the C binary must be at the path
/// returned by `c_binary()`.
fn c_compare_enabled() -> bool {
    std::env::var("TF_C_COMPARE").as_deref() == Ok("1")
}

/// Run a test case: verify Rust output matches `expected`, and (optionally,
/// when `TF_C_COMPARE=1`) that C output also matches.
fn check(script: &str, expected: &[&str]) {
    let rust_bin = rust_binary();
    let raw = run_tf(&rust_bin, script, false);
    let got = normalise_output(&raw);
    let want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();

    assert_eq!(
        got, want,
        "\n--- Rust output mismatch ---\nScript:\n{script}\nGot:\n{got:#?}\nWant:\n{want:#?}\nRaw:\n{raw}"
    );

    // Optional C comparison — only when explicitly enabled.
    if c_compare_enabled() {
        if let Some(c_bin) = c_binary() {
            let c_raw = run_tf(&c_bin, script, true);
            let c_got = normalise_output(&c_raw);
            assert_eq!(
                c_got, want,
                "\n--- C output mismatch ---\nScript:\n{script}\nGot:\n{c_got:#?}\nWant:\n{want:#?}\nRaw:\n{c_raw}"
            );
        }
    }
}

// ── Test cases ────────────────────────────────────────────────────────────────

#[test]
fn echo_simple() {
    check("/echo hello world", &["hello world"]);
}

#[test]
fn arithmetic_expression() {
    check("/echo $[2 + 3 * 4]", &["14"]);
}

#[test]
fn string_concat() {
    check("/echo $[\"foo\" . \"bar\"]", &["foobar"]);
}

#[test]
fn variable_set_and_expand() {
    check(
        "/set x=42\n/echo %x",
        &["42"],
    );
}

#[test]
fn if_true_branch() {
    check(
        "/if (1) /echo yes%; /else /echo no%; /endif",
        &["yes"],
    );
}

#[test]
fn if_false_branch() {
    check(
        "/if (0) /echo yes%; /else /echo no%; /endif",
        &["no"],
    );
}

#[test]
fn for_loop_count() {
    check(
        "/for i 1 3 {/echo %i}",
        &["1", "2", "3"],
    );
}

#[test]
fn while_loop() {
    check(
        "/set n=3\n/while (%n > 0) {/echo %n%; /set n=$[%n-1]}",
        &["3", "2", "1"],
    );
}

#[test]
fn string_function_strlen() {
    check("/echo $[strlen(\"hello\")]", &["5"]);
}

#[test]
fn string_function_toupper() {
    check("/echo $[toupper(\"hello\")]", &["HELLO"]);
}

#[test]
fn string_function_substr() {
    check("/echo $[substr(\"hello\",1,3)]", &["ell"]);
}

#[test]
fn ternary_operator() {
    check("/echo $[1 ? \"yes\" : \"no\"]", &["yes"]);
}

#[test]
fn nested_variable_expansion() {
    check(
        "/set foo=bar\n/set bar=42\n/echo @@foo",
        &["42"],
    );
}

#[test]
fn percent_paren_inline_expr() {
    check(
        "/set n=7\n/echo %(n * 2)",
        &["14"],
    );
}

#[test]
fn positional_args_in_macro() {
    check(
        "/def testmac = /echo {1} {2}\n/testmac hello world",
        &["hello world"],
    );
}

#[test]
fn macro_with_body_executes() {
    check(
        "/def greet = /echo hello %{1}\n/greet world",
        &["hello world"],
    );
}

#[test]
fn string_match_tilde() {
    check(
        r#"/if ("hello world" =~ "hello*") /echo match%; /else /echo no%; /endif"#,
        &["match"],
    );
}

#[test]
fn not_tilde_mismatch() {
    check(
        r#"/if ("hello world" !~ "xyz*") /echo yes%; /else /echo no%; /endif"#,
        &["yes"],
    );
}

#[test]
fn set_and_unset_variable() {
    check(
        "/set myvar=hello\n/unset myvar\n/echo $[isset(\"myvar\")]",
        &["0"],
    );
}

#[test]
fn echo_empty_line() {
    // /echo with no args should output a blank line — after normalisation
    // (which strips empty lines) this produces nothing.  That means both
    // C and Rust should agree: empty output list.
    check("/echo", &[]);
}
