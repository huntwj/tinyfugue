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
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect();

    let mut parser = StmtParser {
        stmts: stmts_raw,
        pos: 0,
    };
    parser.parse_block_until(None)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Join lines that end with `\` into single logical lines.
fn join_continuations(src: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in src.lines() {
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
fn split_by_separator(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_str = false;
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
            '%' if !in_str => {
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
        let cond = strip_parens(rest.trim()).to_owned();

        let then_block = self.parse_block_until(Some(&["else", "endif"]))?;

        // Consume /else or /endif; treat EOF as implicit /endif.
        let terminator = self.advance();
        let tc = terminator.as_deref().map(cmd_name).unwrap_or("endif");

        let else_block = if tc == "else" {
            let blk = self.parse_block_until(Some(&["endif"]))?;
            self.advance(); // consume /endif (or ignore None at EOF)
            blk
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
        let body = parse_script(body_str)?;
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
    let (name, value) = if let Some(eq) = rest.find('=') {
        (rest[..eq].trim().to_owned(), rest[eq + 1..].to_owned())
    } else {
        // `/set name value`
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").trim().to_owned();
        let value = parts.next().unwrap_or("").trim().to_owned();
        (name, value)
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
}
