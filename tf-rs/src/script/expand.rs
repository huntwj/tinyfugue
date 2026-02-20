//! TF text substitution / variable expansion.
//!
//! Handles the substitution sequences that appear in macro bodies and
//! command arguments before execution:
//!
//! | Sequence        | Meaning                                              |
//! |-----------------|------------------------------------------------------|
//! | `%{name}`       | Global/local variable named `name`                  |
//! | `%name`         | Same, alternate form (single identifier token)       |
//! | `{n}` / `%n`   | Positional parameter n (1-based)                    |
//! | `{#}` / `%#`   | Number of positional parameters                     |
//! | `{*}` / `%*`   | All positional parameters joined with spaces         |
//! | `{P}` / `%P`   | Current command/macro name                           |
//! | `$[expr]`       | Evaluate `expr` and substitute the result            |

use super::expr::{EvalContext, eval_str};

/// Expand all substitution sequences in `src`, returning the result.
///
/// Uses `ctx` for variable lookups and expression evaluation.
pub fn expand(src: &str, ctx: &mut dyn EvalContext) -> Result<String, String> {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '%' => {
                match chars.peek().copied() {
                    Some('{') => {
                        // %{name} — consume '{'
                        chars.next();
                        let name = read_brace_name(&mut chars)?;
                        out.push_str(&resolve_brace(name, ctx));
                    }
                    Some('#') => {
                        chars.next();
                        out.push_str(&ctx.positional_params().len().to_string());
                    }
                    Some('*') => {
                        chars.next();
                        out.push_str(&ctx.positional_params().join(" "));
                    }
                    Some('P') => {
                        chars.next();
                        out.push_str(ctx.current_cmd_name());
                    }
                    Some(c) if c.is_ascii_digit() && c != '0' => {
                        chars.next();
                        let mut n_str = String::from(c);
                        while matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                            n_str.push(chars.next().unwrap());
                        }
                        let idx: usize = n_str.parse().unwrap_or(0);
                        let params = ctx.positional_params();
                        out.push_str(params.get(idx.saturating_sub(1)).map(String::as_str).unwrap_or(""));
                    }
                    Some(c) if is_ident_start(c) => {
                        // %name — bare variable name
                        chars.next();
                        let mut name = String::from(c);
                        while matches!(chars.peek(), Some(nc) if is_ident_continue(*nc)) {
                            name.push(chars.next().unwrap());
                        }
                        out.push_str(&lookup_var(&name, ctx));
                    }
                    _ => {
                        // Literal '%'
                        out.push('%');
                    }
                }
            }
            '{' => {
                // {n}, {#}, {*}, {P} — brace-only forms (no leading %)
                let name = read_brace_name(&mut chars)?;
                out.push_str(&resolve_brace(name, ctx));
            }
            '$' => {
                if chars.peek() == Some('[').as_ref() {
                    chars.next(); // consume '['
                    let mut expr_src = String::new();
                    let mut depth = 1usize;
                    for ec in chars.by_ref() {
                        match ec {
                            '[' => { depth += 1; expr_src.push(ec); }
                            ']' => {
                                depth -= 1;
                                if depth == 0 { break; }
                                expr_src.push(ec);
                            }
                            _ => expr_src.push(ec),
                        }
                    }
                    let val = ctx.eval_expr_str(&expr_src)?;
                    out.push_str(&val.to_string());
                } else {
                    out.push('$');
                }
            }
            other => out.push(other),
        }
    }

    Ok(out)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Read everything up to and including the closing `}`.
fn read_brace_name(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, String> {
    let mut name = String::new();
    loop {
        match chars.next() {
            Some('}') => break,
            Some(c)   => name.push(c),
            None      => return Err("unclosed '{'".into()),
        }
    }
    Ok(name)
}

/// Resolve a `{name}` or `%{name}` expression.
fn resolve_brace(name: String, ctx: &mut dyn EvalContext) -> String {
    match name.as_str() {
        "#" => ctx.positional_params().len().to_string(),
        "*" => ctx.positional_params().join(" "),
        "P" => ctx.current_cmd_name().to_owned(),
        other => {
            // Numeric: positional parameter
            if let Ok(n) = other.parse::<usize>() {
                let params = ctx.positional_params();
                return params.get(n.saturating_sub(1)).cloned().unwrap_or_default();
            }
            lookup_var(other, ctx)
        }
    }
}

fn lookup_var(name: &str, ctx: &dyn EvalContext) -> String {
    ctx.get_var(name)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// Convenience: expand `src` using only a simple variable map (no expressions).
pub fn expand_simple<F>(src: &str, lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    struct SimpleCtx<F: Fn(&str) -> Option<String>>(F);

    impl<F: Fn(&str) -> Option<String>> EvalContext for SimpleCtx<F> {
        fn get_var(&self, name: &str) -> Option<super::value::Value> {
            self.0(name).map(super::value::Value::Str)
        }
        fn set_local(&mut self, _: &str, _: super::value::Value) {}
        fn set_global(&mut self, _: &str, _: super::value::Value) {}
        fn positional_params(&self) -> &[String] { &[] }
        fn current_cmd_name(&self) -> &str { "" }
        fn call_fn(&mut self, name: &str, _: Vec<super::value::Value>) -> Result<super::value::Value, String> {
            Err(format!("unknown function {name}"))
        }
        fn eval_expr_str(&mut self, s: &str) -> Result<super::value::Value, String> {
            eval_str(s, self)
        }
    }

    expand(src, &mut SimpleCtx(lookup)).unwrap_or_else(|_| src.to_owned())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::value::Value;
    use std::collections::HashMap;

    struct TestCtx {
        vars: HashMap<String, Value>,
        params: Vec<String>,
        cmd: String,
    }

    impl TestCtx {
        fn new() -> Self {
            TestCtx {
                vars: HashMap::new(),
                params: Vec::new(),
                cmd: String::new(),
            }
        }
    }

    impl EvalContext for TestCtx {
        fn get_var(&self, name: &str) -> Option<Value> { self.vars.get(name).cloned() }
        fn set_local(&mut self, name: &str, value: Value) { self.vars.insert(name.into(), value); }
        fn set_global(&mut self, name: &str, value: Value) { self.vars.insert(name.into(), value); }
        fn positional_params(&self) -> &[String] { &self.params }
        fn current_cmd_name(&self) -> &str { &self.cmd }
        fn call_fn(&mut self, _: &str, _: Vec<Value>) -> Result<Value, String> { Err("no fns".into()) }
        fn eval_expr_str(&mut self, s: &str) -> Result<Value, String> {
            use super::super::expr::eval_str;
            eval_str(s, self)
        }
    }

    fn exp(src: &str, ctx: &mut TestCtx) -> String {
        expand(src, ctx).expect("expand failed")
    }

    #[test]
    fn no_substitution() {
        let mut ctx = TestCtx::new();
        assert_eq!(exp("hello world", &mut ctx), "hello world");
    }

    #[test]
    fn percent_brace_var() {
        let mut ctx = TestCtx::new();
        ctx.vars.insert("name".into(), Value::Str("Alice".into()));
        assert_eq!(exp("Hello, %{name}!", &mut ctx), "Hello, Alice!");
    }

    #[test]
    fn bare_percent_var() {
        let mut ctx = TestCtx::new();
        ctx.vars.insert("x".into(), Value::Int(42));
        assert_eq!(exp("value=%x end", &mut ctx), "value=42 end");
    }

    #[test]
    fn positional_params() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["foo".into(), "bar".into()];
        assert_eq!(exp("{1} {2}", &mut ctx), "foo bar");
        assert_eq!(exp("%1 %2", &mut ctx), "foo bar");
    }

    #[test]
    fn param_count() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(exp("{#}", &mut ctx), "3");
        assert_eq!(exp("%#", &mut ctx), "3");
    }

    #[test]
    fn param_star() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["x".into(), "y".into()];
        assert_eq!(exp("{*}", &mut ctx), "x y");
        assert_eq!(exp("%*", &mut ctx), "x y");
    }

    #[test]
    fn cmd_name() {
        let mut ctx = TestCtx::new();
        ctx.cmd = "mycmd".into();
        assert_eq!(exp("{P}", &mut ctx), "mycmd");
        assert_eq!(exp("%P", &mut ctx), "mycmd");
    }

    #[test]
    fn expr_substitution() {
        let mut ctx = TestCtx::new();
        assert_eq!(exp("result=$[2 + 3]", &mut ctx), "result=5");
    }

    #[test]
    fn literal_percent() {
        let mut ctx = TestCtx::new();
        // A bare % not followed by a recognized sequence stays as-is.
        assert_eq!(exp("100%!", &mut ctx), "100%!");
    }

    #[test]
    fn expand_simple_fn() {
        let result = expand_simple("Hello, %{name}!", |k| {
            if k == "name" { Some("World".into()) } else { None }
        });
        assert_eq!(result, "Hello, World!");
    }

    #[test]
    fn missing_var_expands_empty() {
        let mut ctx = TestCtx::new();
        assert_eq!(exp("%{nosuchvar}", &mut ctx), "");
    }
}
