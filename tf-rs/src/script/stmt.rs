//! TF statement AST and script-level parser.
//!
//! A TF script is a sequence of lines.  Each non-empty, non-comment line is
//! either a command invocation (`/keyword args`) or a bare string (sent to the
//! current world).  Continuation lines ending in `\` are joined.  Multiple
//! statements on one line may be separated by `%;`.

/// A parsed TF statement.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `/if (cond) then-block [/else else-block] /endif`
    If {
        cond: String,
        then_block: Vec<Stmt>,
        else_block: Vec<Stmt>,
    },
    /// `/while (cond) body /done`
    While { cond: String, body: Vec<Stmt> },
    /// `/for var start end body`  — iterates var from start to end (inclusive)
    For {
        var: String,
        start: String,
        end: String,
        body: Vec<Stmt>,
    },
    /// `/let name=value`
    Let { name: String, value: String },
    /// `/set name=value` or `/set name value`
    Set { name: String, value: String },
    /// `/unset name`
    Unset { name: String },
    /// `/return [expr]`
    Return { value: Option<String> },
    /// `/break`
    Break,
    /// `/echo [-n] text`
    Echo { text: String, newline: bool },
    /// `/send text`
    Send { text: String },
    /// `/expr expression`
    Expr { src: String },
    /// `/addworld ...` (forwarded to config layer at runtime)
    AddWorld { args: Vec<String> },
    /// Any other `/command args` that we don't understand structurally.
    Command { name: String, args: String },
    /// A bare line (not starting with `/`) — sent to the current world.
    Raw(String),
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a TF script string into a list of statements.
///
/// The parser handles:
/// - Line continuation with `\` at end of line
/// - Statement separator `%;`
/// - Nested `/if`…`/endif` and `/while`…`/done`
pub fn parse_script(src: &str) -> Result<Vec<Stmt>, String> {
    let lines = join_continuations(src);
    let stmts_raw: Vec<String> = lines
        .iter()
        .flat_map(|l| split_by_separator(l))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && !s.starts_with('#') && !s.starts_with(';'))
        .collect();

    let mut parser = StmtParser {
        stmts: stmts_raw,
        pos: 0,
    };
    parser.parse_block_until(None)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Join lines that end with `\` into single logical lines.
///
/// TF `;`-comment lines (starting with `;`) are skipped without terminating
/// a continuation sequence — they behave like blank lines.
fn join_continuations(src: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in src.lines() {
        // Skip TF semicolon-comment lines without breaking continuation.
        if line.trim_start().starts_with(';') {
            continue;
        }
        if let Some(stripped) = line.strip_suffix('\\') {
            current.push_str(stripped);
        } else {
            current.push_str(line);
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Split a logical line on `%;` (but not inside strings).
///
/// Body commands (`/def`, `/trigger`, `/hook`, `/alias`, `/bind`) use `=` to
/// separate the macro name from its body.  The body itself may contain `%;`
/// as intra-body statement separators, which must **not** be treated as outer
/// statement separators.  These commands are therefore returned as a single
/// unsplit element.
fn split_by_separator(line: &str) -> Vec<String> {
    // If the line is a body command, return it whole — the %; inside the body
    // are intra-body separators, not outer ones.
    if is_body_cmd(line) {
        let trimmed = line.trim().to_owned();
        return if trimmed.is_empty() { vec![] } else { vec![trimmed] };
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_str = false;
    let mut brace_depth = 0usize;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_str = !in_str;
                current.push(ch);
            }
            '\\' if in_str => {
                current.push(ch);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '{' if !in_str => {
                brace_depth += 1;
                current.push(ch);
            }
            '}' if !in_str => {
                brace_depth = brace_depth.saturating_sub(1);
                current.push(ch);
            }
            '%' if !in_str && brace_depth == 0 => {
                if chars.peek() == Some(&';') {
                    chars.next();
                    parts.push(std::mem::take(&mut current));
                } else {
                    current.push(ch);
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

/// Returns `true` if the logical line is a command whose body (after `=`) must
/// not be split on `%;`.  These are macro-defining commands.
///
/// # Abbreviations
///
/// Only the full command names are checked here.  The TF scripting parser does
/// **not** support abbreviated commands (e.g. `/d` for `/def`) inside compound
/// statements, so no abbreviation expansion is needed.  If abbreviated forms are
/// ever added to the interpreter, this function must be updated in sync.
fn is_body_cmd(line: &str) -> bool {
    let s = line.trim_start();
    if !s.starts_with('/') {
        return false;
    }
    let name = s[1..].split_whitespace().next().unwrap_or("");
    matches!(name, "def" | "trigger" | "hook" | "alias" | "bind")
}

// ── Statement-level parser ────────────────────────────────────────────────────

struct StmtParser {
    stmts: Vec<String>,
    pos: usize,
}

impl StmtParser {
    fn peek(&self) -> Option<&str> {
        self.stmts.get(self.pos).map(String::as_str)
    }

    fn advance(&mut self) -> Option<String> {
        let s = self.stmts.get(self.pos).cloned();
        if s.is_some() {
            self.pos += 1;
        }
        s
    }

    /// Parse statements until we hit one of the terminator keywords (or EOF).
    ///
    /// `stop_at` is matched against the *command name* (e.g. `"endif"`,
    /// `"done"`, `"else"`).  The terminator is **consumed** unless it's
    /// `"else"` (because `/else` itself starts the else block).
    fn parse_block_until(&mut self, stop_at: Option<&[&str]>) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();
        loop {
            let line = match self.peek() {
                None => {
                    // EOF implicitly closes any open block (needed for multi-file /load sourcing).
                    break;
                }
                Some(l) => l.to_owned(),
            };

            // Check if this line is a terminator.
            if let Some(stops) = stop_at {
                let cmd = cmd_name(&line);
                if stops.contains(&cmd) {
                    // Don't advance — caller handles terminator consumption.
                    break;
                }
            }

            self.pos += 1;
            stmts.push(self.parse_one(&line)?);
        }
        Ok(stmts)
    }

    fn parse_one(&mut self, line: &str) -> Result<Stmt, String> {
        if !line.starts_with('/') {
            return Ok(Stmt::Raw(line.to_owned()));
        }

        let (name, rest) = split_cmd(line);

        match name {
            "if" => self.parse_if(rest),
            "while" => self.parse_while(rest),
            "for" => self.parse_for(rest),
            "let" => Ok(parse_let_or_set(rest, true)),
            "set" => Ok(parse_let_or_set(rest, false)),
            "unset" => Ok(Stmt::Unset {
                name: rest.trim().to_owned(),
            }),
            "return" => {
                let v = rest.trim();
                Ok(Stmt::Return {
                    value: if v.is_empty() {
                        None
                    } else {
                        Some(v.to_owned())
                    },
                })
            }
            "break" => Ok(Stmt::Break),
            "echo" => {
                let (newline, text) = if let Some(stripped) = rest.strip_prefix("-n ") {
                    (false, stripped.to_owned())
                } else {
                    (true, rest.to_owned())
                };
                Ok(Stmt::Echo { text, newline })
            }
            "send" => Ok(Stmt::Send {
                text: rest.to_owned(),
            }),
            "expr" => Ok(Stmt::Expr {
                src: rest.to_owned(),
            }),
            "addworld" => {
                let args = rest.split_whitespace().map(str::to_owned).collect();
                Ok(Stmt::AddWorld { args })
            }
            other => Ok(Stmt::Command {
                name: other.to_owned(),
                args: rest.to_owned(),
            }),
        }
    }

    fn parse_if(&mut self, rest: &str) -> Result<Stmt, String> {
        // Extract condition and optional inline body.
        // E.g. "(_required)         /exit" → cond="_required", body="/exit"
        //      "(x > 0)"                  → cond="x > 0",      body=""
        //      "/@test ..."               → cond="/@test ...",  body=""
        let (cond_str, inline_body) = extract_cond_and_body(rest.trim());
        let cond = cond_str.to_owned();

        // If there's an inline body it forms the start of the then_block.
        let mut then_block = if !inline_body.is_empty() {
            parse_script(inline_body)?
        } else {
            Vec::new()
        };

        // Read further block statements until else / elseif / endif.
        let more = self.parse_block_until(Some(&["else", "elseif", "endif"]))?;
        then_block.extend(more);

        // Consume /else, /elseif, or /endif; treat EOF as implicit /endif.
        let terminator = self.advance();
        let tc = terminator.as_deref().map(cmd_name).unwrap_or("endif");

        let else_block = if tc == "else" {
            // Handle inline body on the /else line: "/else /echo no"
            let mut blk = Vec::new();
            if let Some(term_line) = &terminator {
                let (_, else_inline) = split_cmd(term_line);
                if !else_inline.is_empty() {
                    blk.extend(parse_script(else_inline)?);
                }
            }
            blk.extend(self.parse_block_until(Some(&["endif"]))?);
            self.advance(); // consume /endif (or ignore None at EOF)
            blk
        } else if tc == "elseif" {
            // Treat /elseif as a nested /if in the else branch.
            let term_line = terminator.as_deref().unwrap_or("");
            let (_, elseif_rest) = split_cmd(term_line);
            vec![self.parse_if(elseif_rest)?]
        } else {
            Vec::new() // was /endif or EOF
        };

        Ok(Stmt::If {
            cond,
            then_block,
            else_block,
        })
    }

    fn parse_while(&mut self, rest: &str) -> Result<Stmt, String> {
        // Inline form: /while (cond) {body}
        if let Some((cond_part, brace_part)) = split_cond_and_brace_body(rest) {
            let cond = strip_parens(cond_part.trim()).to_owned();
            let inner = extract_brace_body(brace_part)?;
            let body = parse_script(inner)?;
            return Ok(Stmt::While { cond, body });
        }
        // Multi-line form: /while (cond) ... /done
        let cond = strip_parens(rest.trim()).to_owned();
        let body = self.parse_block_until(Some(&["done"]))?;
        self.advance(); // consume /done
        Ok(Stmt::While { cond, body })
    }

    fn parse_for(&mut self, rest: &str) -> Result<Stmt, String> {
        // TF /for syntax: /for var start end body_statement
        // var, start, end are whitespace-delimited tokens; body is the remainder.
        let (var, rest) = split_word(rest)
            .ok_or_else(|| format!("missing var in /for: {rest}"))?;
        let (start, rest) = split_word(rest)
            .ok_or_else(|| format!("missing start in /for {var}"))?;
        let (end, body_str) = split_word(rest)
            .ok_or_else(|| format!("missing end in /for {var}"))?;
        let body_str = body_str.trim();
        let body = if body_str.starts_with('{') {
            // Inline brace body: /for i 1 3 {/echo %i}
            let inner = extract_brace_body(body_str)?;
            parse_script(inner)?
        } else {
            parse_script(body_str)?
        };
        Ok(Stmt::For {
            var: var.to_owned(),
            start: start.to_owned(),
            end: end.to_owned(),
            body,
        })
    }
}

// ── Small utilities ───────────────────────────────────────────────────────────

/// Extract the command name from a `/cmd args` line.
fn cmd_name(line: &str) -> &str {
    let line = line.trim_start_matches('/');
    line.split_whitespace().next().unwrap_or("")
}

/// Split `/cmd rest` → `("cmd", "rest")`.
fn split_cmd(line: &str) -> (&str, &str) {
    let line = line.trim_start_matches('/');
    match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i + 1..].trim_start()),
        None => (line, ""),
    }
}

/// Remove surrounding `(...)` if present.
fn strip_parens(s: &str) -> &str {
    if s.starts_with('(') && s.ends_with(')') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Extract the content of a `{...}` brace body, returning the inner text.
///
/// Handles balanced inner braces.  `s` must start with `{` and end with `}`.
fn extract_brace_body(s: &str) -> Result<&str, String> {
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return Err(format!("expected {{...}} body, got: {s}"));
    }
    Ok(&s[1..s.len() - 1])
}

/// If `rest` has an inline `{...}` body (i.e. ends with `}`), split it into
/// `(condition_part, brace_block_str)`.  Returns `None` if there is no inline
/// brace body (multi-line `/done`-terminated form).
fn split_cond_and_brace_body(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim();
    if !rest.ends_with('}') {
        return None;
    }
    // Find the opening `{` at paren-depth == 0 (not inside `(...)` or strings).
    let mut paren_depth = 0i32;
    let mut in_str = false;
    for (i, ch) in rest.char_indices() {
        match ch {
            '"' | '\'' => in_str = !in_str,
            '(' if !in_str => paren_depth += 1,
            ')' if !in_str => paren_depth -= 1,
            '{' if !in_str && paren_depth == 0 => {
                return Some((&rest[..i], &rest[i..]));
            }
            _ => {}
        }
    }
    None
}

/// Split a `/if` or `/elseif` argument into `(condition, inline_body)`.
///
/// If `s` starts with `(`, the condition is the text inside the first matching
/// pair of parentheses; anything after the closing `)` (trimmed) is the inline
/// body.  Otherwise the entire `s` is the condition and the body is empty.
///
/// Examples:
/// - `"(x > 0)"` → `("x > 0", "")`
/// - `"(x > 0) /echo hi"` → `("x > 0", "/echo hi")`
/// - `"/@test foo !/ bar"` → `("/@test foo !/ bar", "")`
fn extract_cond_and_body(s: &str) -> (&str, &str) {
    if !s.starts_with('(') {
        return (s, "");
    }
    // Find the matching close paren (depth-counting for nested parens).
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let cond = &s[1..i];
                    let body = s[i + 1..].trim_start();
                    return (cond, body);
                }
            }
            _ => {}
        }
    }
    // No matching close paren — treat whole string as condition.
    (s, "")
}

/// Split off the first whitespace-delimited word from `s`.
/// Returns `(word, rest)` or `None` if `s` is empty/all-whitespace.
fn split_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// Parse `/let name=value` or `/set name=value` or `/set name value`.
fn parse_let_or_set(rest: &str, is_let: bool) -> Stmt {
    // Read the variable name: alphanumeric + underscore only (matches C TF's spanvar).
    // This prevents splitting on '=' that appears inside the value (e.g. "=~" in expressions).
    let name_end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let name = rest[..name_end].to_owned();
    let after_name = &rest[name_end..];

    // C TF's setdelim logic:
    //   starts with '='  → value is everything after '='
    //   starts with space → skip whitespace; rest is the value
    //   empty             → no value (display/unset behaviour)
    let value = if let Some(after_eq) = after_name.strip_prefix('=') {
        after_eq.to_owned()
    } else {
        after_name.trim_start().to_owned()
    };

    if is_let {
        Stmt::Let { name, value }
    } else {
        Stmt::Set { name, value }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Vec<Stmt> {
        parse_script(src).expect("parse failed")
    }

    #[test]
    fn empty() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn raw_line() {
        let stmts = parse("go east");
        assert!(matches!(&stmts[0], Stmt::Raw(s) if s == "go east"));
    }

    #[test]
    fn echo_stmt() {
        let stmts = parse("/echo Hello, world!");
        assert!(matches!(&stmts[0], Stmt::Echo { text, newline: true } if text == "Hello, world!"));
    }

    #[test]
    fn echo_no_newline() {
        let stmts = parse("/echo -n no newline here");
        assert!(matches!(
            &stmts[0],
            Stmt::Echo {
                text: _,
                newline: false
            }
        ));
    }

    #[test]
    fn set_eq_form() {
        let stmts = parse("/set wrap=1");
        assert!(matches!(&stmts[0], Stmt::Set { name, value } if name == "wrap" && value == "1"));
    }

    #[test]
    fn set_space_form() {
        let stmts = parse("/set wrap 1");
        assert!(matches!(&stmts[0], Stmt::Set { name, value } if name == "wrap" && value == "1"));
    }

    #[test]
    fn return_with_value() {
        let stmts = parse("/return 42");
        assert!(matches!(&stmts[0], Stmt::Return { value: Some(v) } if v == "42"));
    }

    #[test]
    fn return_no_value() {
        let stmts = parse("/return");
        assert!(matches!(&stmts[0], Stmt::Return { value: None }));
    }

    #[test]
    fn if_endif() {
        let src = "/if (x > 0)\n/echo positive\n/endif";
        let stmts = parse(src);
        assert_eq!(stmts.len(), 1);
        if let Stmt::If {
            cond,
            then_block,
            else_block,
        } = &stmts[0]
        {
            assert_eq!(cond, "x > 0");
            assert_eq!(then_block.len(), 1);
            assert!(else_block.is_empty());
        } else {
            panic!("expected If");
        }
    }

    #[test]
    fn if_else_endif() {
        let src = "/if (x > 0)\n/echo pos\n/else\n/echo neg\n/endif";
        let stmts = parse(src);
        if let Stmt::If {
            cond,
            then_block,
            else_block,
        } = &stmts[0]
        {
            assert_eq!(cond, "x > 0");
            assert_eq!(then_block.len(), 1);
            assert_eq!(else_block.len(), 1);
        } else {
            panic!("expected If");
        }
    }

    #[test]
    fn while_done() {
        let src = "/while (i < 10)\n/set i=%{i}+1\n/done";
        let stmts = parse(src);
        assert!(matches!(&stmts[0], Stmt::While { .. }));
    }

    #[test]
    fn semicolon_separator() {
        let src = "/echo one%;/echo two";
        let stmts = parse(src);
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn line_continuation() {
        let src = "/echo hello \\\nworld";
        let stmts = parse(src);
        assert_eq!(stmts.len(), 1);
        if let Stmt::Echo { text, .. } = &stmts[0] {
            assert!(text.contains("hello"));
        }
    }

    #[test]
    fn comment_skipped() {
        let stmts = parse("# this is a comment\n/echo hi");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn break_stmt() {
        let stmts = parse("/break");
        assert!(matches!(stmts[0], Stmt::Break));
    }

    #[test]
    fn unknown_command() {
        let stmts = parse("/def foo=bar");
        assert!(matches!(&stmts[0], Stmt::Command { name, .. } if name == "def"));
    }

    #[test]
    fn eof_closes_if_block() {
        // TF treats EOF as an implicit /endif — needed for multi-file /load sourcing.
        let stmts = parse_script("/if (x > 0)\n/echo hi").expect("parse failed");
        assert_eq!(stmts.len(), 1);
        assert!(matches!(&stmts[0], Stmt::If { .. }));
    }

    #[test]
    fn eof_closes_while_block() {
        // TF treats EOF as an implicit /done.
        let stmts = parse_script("/while (i < 3)\n/echo hi").expect("parse failed");
        assert_eq!(stmts.len(), 1);
        assert!(matches!(&stmts[0], Stmt::While { .. }));
    }

    #[test]
    fn for_range_inline_body() {
        // /for var start end body  (TF range syntax, body on same line)
        let stmts = parse("/for i 0 5 /echo %i");
        assert_eq!(stmts.len(), 1);
        if let Stmt::For { var, start, end, body } = &stmts[0] {
            assert_eq!(var, "i");
            assert_eq!(start, "0");
            assert_eq!(end, "5");
            assert_eq!(body.len(), 1);
            assert!(matches!(&body[0], Stmt::Echo { .. }));
        } else {
            panic!("expected For");
        }
    }

    #[test]
    fn for_range_nested() {
        // Nested /for via continuation joining: /for x 0 2 /for y 0 2 /echo ...
        let stmts = parse("/for x 0 2 /for y 0 2 /echo xy");
        assert_eq!(stmts.len(), 1);
        if let Stmt::For { var, body, .. } = &stmts[0] {
            assert_eq!(var, "x");
            assert_eq!(body.len(), 1);
            assert!(matches!(&body[0], Stmt::For { .. }));
        } else {
            panic!("expected outer For");
        }
    }

    #[test]
    fn semicolon_comment_skipped() {
        // TF ';' comments are skipped like '#' comments.
        let stmts = parse("; this is a tf comment\n/echo hi");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn semicolon_comment_preserves_continuation() {
        // A ';' comment in the middle of a continuation should NOT break it.
        let src = "/def foo = \\\n; comment here\n/echo body";
        let stmts = parse(src);
        // The /def should absorb the continuation including the echo.
        assert_eq!(stmts.len(), 1);
        assert!(matches!(&stmts[0], Stmt::Command { name, .. } if name == "def"));
    }

    #[test]
    fn def_body_not_split() {
        // /def body containing %; must not be split into multiple statements.
        let src = "/def foo = /echo one%; /echo two";
        let stmts = parse(src);
        assert_eq!(stmts.len(), 1, "def body should not be split on %;");
        assert!(matches!(&stmts[0], Stmt::Command { name, .. } if name == "def"));
    }

    #[test]
    fn if_elseif_endif() {
        let src = "/if (x > 0)\n/echo pos\n/elseif (x < 0)\n/echo neg\n/endif";
        let stmts = parse(src);
        assert_eq!(stmts.len(), 1);
        if let Stmt::If { cond, then_block, else_block } = &stmts[0] {
            assert_eq!(cond, "x > 0");
            assert_eq!(then_block.len(), 1);
            // else_block should contain a nested If for the elseif
            assert_eq!(else_block.len(), 1);
            assert!(matches!(&else_block[0], Stmt::If { .. }));
        } else {
            panic!("expected If");
        }
    }

    #[test]
    fn if_with_inline_body() {
        // /if (cond) stmt — inline body form
        let src = "/if (x > 0) /echo pos%; /endif";
        let stmts = parse(src);
        assert_eq!(stmts.len(), 1);
        if let Stmt::If { cond, then_block, .. } = &stmts[0] {
            assert_eq!(cond, "x > 0");
            assert!(!then_block.is_empty());
        } else {
            panic!("expected If");
        }
    }
}
