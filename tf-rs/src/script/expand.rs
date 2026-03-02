//! TF text substitution / variable expansion.
//!
//! Handles the substitution sequences that appear in macro bodies and
//! command arguments before execution:
//!
//! | Sequence           | Meaning                                              |
//! |--------------------|------------------------------------------------------|
//! | `%{name}`          | Global/local variable named `name`                  |
//! | `%name`            | Same, alternate form (single identifier token)       |
//! | `${name}`          | Same, dollar-brace form                              |
//! | `{n}` / `%n`       | Positional parameter n (1-based)                    |
//! | `{#}` / `%#`       | Number of positional parameters                     |
//! | `{*}` / `%*`       | All positional parameters joined with spaces         |
//! | `{L}` / `%L`       | Last positional parameter                            |
//! | `{-N}`             | All positional parameters from index N onward        |
//! | `{-L}`             | All positional parameters except the last            |
//! | `%-N`              | Same as `{-N}` (bare form)                           |
//! | `%-L`              | Same as `{-L}` (bare form)                           |
//! | `{name-default}`   | Variable `name`, or `default` if unset/empty         |
//! | `{N-default}`      | Positional param N, or `default` if missing          |
//! | `{L-default}`      | Last param, or `default` if no params                |
//! | `{-L-default}`     | All-but-last params, or `default` if ≤1 params       |
//! | `{-N-default}`     | Params[N..], or `default` if too few params          |
//! | `{*-default}`      | All params joined, or `default` if no params         |
//! | `{P}` / `%P`       | Current command/macro name                           |
//! | `$[expr]`          | Evaluate `expr` and substitute the result            |
//! | `%(expr)`          | Same as `$[expr]` — alternate inline-expression form |
//! | `@@varname`        | Indirect expansion: look up %varname, then look up the resulting name |
//! | `$$`               | Literal `$`                                          |

use super::expr::{EvalContext, eval_str};

/// Expand all substitution sequences in `src`, returning the result.
///
/// Uses `ctx` for variable lookups and expression evaluation.
pub fn expand(src: &str, ctx: &mut dyn EvalContext) -> Result<String, String> {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '@' => {
                // @@varname — indirect expansion: expand %varname, then use that
                // value as a variable name and look up *its* value.
                // Mirrors C TF expand.c `@@` handling.
                if chars.peek().copied() == Some('@') {
                    chars.next(); // consume second '@'
                    // Read the variable name (identifier chars).
                    let mut name = String::new();
                    if matches!(chars.peek(), Some(c) if is_ident_start(*c)) {
                        name.push(chars.next().unwrap());
                        while matches!(chars.peek(), Some(c) if is_ident_continue(*c)) {
                            name.push(chars.next().unwrap());
                        }
                    }
                    // First dereference: value of %name.
                    let inner = lookup_var(&name, ctx);
                    // Second dereference: value of %<inner>.
                    out.push_str(&lookup_var(&inner, ctx));
                } else {
                    out.push('@');
                }
            }
            '%' => {
                match chars.peek().copied() {
                    Some('(') => {
                        // %(expr) — inline expression, equivalent to $[expr].
                        // Mirrors C TF expand.c '%' '(' case.
                        chars.next(); // consume '('
                        let mut expr_src = String::new();
                        let mut depth = 1usize;
                        for ec in chars.by_ref() {
                            match ec {
                                '(' => { depth += 1; expr_src.push(ec); }
                                ')' => {
                                    depth -= 1;
                                    if depth == 0 { break; }
                                    expr_src.push(ec);
                                }
                                _ => expr_src.push(ec),
                            }
                        }
                        let val = ctx.eval_expr_str(&expr_src)?;
                        out.push_str(&val.to_string());
                    }
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
                    Some('L') => {
                        chars.next();
                        let params = ctx.positional_params();
                        if let Some(last) = params.last() {
                            out.push_str(last);
                        }
                    }
                    Some('-') => {
                        // %-L (all-but-last) or %-N (all-but-first-N)
                        chars.next(); // consume '-'
                        match chars.peek().copied() {
                            Some('L') => {
                                chars.next(); // consume 'L'
                                let params = ctx.positional_params().to_vec();
                                if params.len() > 1 {
                                    out.push_str(&params[..params.len() - 1].join(" "));
                                }
                            }
                            Some(d) if d.is_ascii_digit() => {
                                let mut n_str = String::new();
                                while matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                                    n_str.push(chars.next().unwrap());
                                }
                                if let Ok(n) = n_str.parse::<usize>() {
                                    let params = ctx.positional_params().to_vec();
                                    if n < params.len() {
                                        out.push_str(&params[n..].join(" "));
                                    }
                                }
                            }
                            _ => {
                                // Unrecognised %- sequence — output literally.
                                out.push('%');
                                out.push('-');
                            }
                        }
                    }
                    Some(c) if c.is_ascii_digit() && c != '0' => {
                        chars.next();
                        let mut n_str = String::from(c);
                        while matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                            n_str.push(chars.next().unwrap());
                        }
                        let idx: usize = n_str.parse().unwrap_or(0);
                        let params = ctx.positional_params();
                        out.push_str(
                            params.get(idx.saturating_sub(1)).map(String::as_str).unwrap_or(""),
                        );
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
                // {n}, {#}, {*}, {P}, {L}, {-N}, … — brace-only forms (no leading %)
                let name = read_brace_name(&mut chars)?;
                out.push_str(&resolve_brace(name, ctx));
            }
            '$' => {
                match chars.peek().copied() {
                    Some('[') => {
                        chars.next(); // consume '['
                        let mut expr_src = String::new();
                        let mut depth = 1usize;
                        for ec in chars.by_ref() {
                            match ec {
                                '[' => {
                                    depth += 1;
                                    expr_src.push(ec);
                                }
                                ']' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    expr_src.push(ec);
                                }
                                _ => expr_src.push(ec),
                            }
                        }
                        // Pre-expand %var references inside the expression
                        // so that "$[%n-1]" correctly substitutes %n before
                        // the expression evaluator runs (which treats % as modulo).
                        let expanded_src = expand(&expr_src, ctx)?;
                        let val = ctx.eval_expr_str(&expanded_src)?;
                        out.push_str(&val.to_string());
                    }
                    Some('{') => {
                        // ${name} — same as %{name}
                        chars.next(); // consume '{'
                        let name = read_brace_name(&mut chars)?;
                        out.push_str(&resolve_brace(name, ctx));
                    }
                    Some('$') => {
                        // $$ — escaped dollar sign
                        chars.next(); // consume second '$'
                        out.push('$');
                    }
                    _ => {
                        out.push('$');
                    }
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

/// Read everything up to and including the closing `}`, tracking nested
/// `{...}` depth so that `%{foo-%{bar}}` correctly yields `foo-%{bar}`.
fn read_brace_name(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, String> {
    let mut name = String::new();
    let mut depth = 0i32; // depth inside any nested braces
    loop {
        match chars.next() {
            Some('}') => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                name.push('}');
            }
            Some('{') => {
                depth += 1;
                name.push('{');
            }
            Some(c) => name.push(c),
            None => return Err("unclosed '{'".into()),
        }
    }
    Ok(name)
}

/// Resolve a `{name}` / `%{name}` / `${name}` expression.
///
/// Handles the full TF positional-argument form, default-value syntax, and
/// plain variable lookup — see module doc-comment for the complete table.
fn resolve_brace(name: String, ctx: &mut dyn EvalContext) -> String {
    // Clone params up-front so we can borrow `ctx` mutably later.
    let params: Vec<String> = ctx.positional_params().to_vec();

    // ── Fast path for common atomic forms ────────────────────────────────────
    match name.as_str() {
        "#" => return params.len().to_string(),
        "*" => return params.join(" "),
        "P" => return ctx.current_cmd_name().to_owned(),
        "L" => return params.last().cloned().unwrap_or_default(),
        "-L" => {
            return if params.len() > 1 {
                params[..params.len() - 1].join(" ")
            } else {
                String::new()
            };
        }
        _ => {}
    }

    // ── `-prefix` forms: {-L}, {-L-default}, {-N}, {-N-default} ─────────────
    if let Some(rest) = name.strip_prefix('-') {
        // {-L} or {-L-<default>}
        if rest == "L" || rest.starts_with("L-") {
            let all_but_last = if params.len() > 1 {
                Some(params[..params.len() - 1].join(" "))
            } else {
                None
            };
            return if let Some(v) = all_but_last {
                v
            } else if let Some(default_str) = rest.strip_prefix("L-") {
                expand_default(default_str, ctx)
            } else {
                String::new()
            };
        }

        // {-N} or {-N-<default>}
        let (n_str, opt_default) = split_at_dash(rest);
        if let Ok(n) = n_str.parse::<usize>() {
            let slice: Vec<&String> = if n < params.len() {
                params[n..].iter().collect()
            } else {
                Vec::new()
            };
            return if !slice.is_empty() {
                slice.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
            } else if let Some(default_str) = opt_default {
                expand_default(default_str, ctx)
            } else {
                String::new()
            };
        }

        // Unrecognised -{…} — return empty.
        return String::new();
    }

    // ── `*` forms: {*}, {*-<default>} ────────────────────────────────────────
    if name == "*" {
        return params.join(" ");
    }
    if let Some(default_str) = name.strip_prefix("*-") {
        return if !params.is_empty() {
            params.join(" ")
        } else {
            expand_default(default_str, ctx)
        };
    }

    // ── Numeric positional or {N-<default>} ──────────────────────────────────
    let (key, opt_default) = split_at_dash(&name);

    if let Ok(n) = key.parse::<usize>() {
        let val = params.get(n.saturating_sub(1)).cloned();
        return if let Some(v) = val {
            v
        } else if let Some(default_str) = opt_default {
            expand_default(default_str, ctx)
        } else {
            String::new()
        };
    }

    // ── `L` with optional default: {L-<default>} ─────────────────────────────
    if key == "L" {
        let val = params.last().cloned();
        return if let Some(v) = val {
            v
        } else if let Some(default_str) = opt_default {
            expand_default(default_str, ctx)
        } else {
            String::new()
        };
    }

    // ── Variable lookup with optional default ─────────────────────────────────
    let val = ctx.get_var(key).map(|v| v.to_string());
    match (val.as_deref(), opt_default) {
        (Some(v), _) if !v.is_empty() => v.to_owned(),
        (_, Some(default_str)) => expand_default(default_str, ctx),
        _ => String::new(),
    }
}

/// Split `s` at the first `-`, returning `(before, Some(after))` or
/// `(s, None)` if there is no `-`.
fn split_at_dash(s: &str) -> (&str, Option<&str>) {
    match s.find('-') {
        Some(pos) => (&s[..pos], Some(&s[pos + 1..])),
        None => (s, None),
    }
}

/// Expand a default-value string (may itself contain `%{…}` sequences).
fn expand_default(s: &str, ctx: &mut dyn EvalContext) -> String {
    expand(s, ctx).unwrap_or_else(|_| s.to_owned())
}

fn lookup_var(name: &str, ctx: &dyn EvalContext) -> String {
    ctx.get_var(name).map(|v| v.to_string()).unwrap_or_default()
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
        fn positional_params(&self) -> &[String] {
            &[]
        }
        fn current_cmd_name(&self) -> &str {
            ""
        }
        fn call_fn(
            &mut self,
            name: &str,
            _: Vec<super::value::Value>,
        ) -> Result<super::value::Value, String> {
            Err(format!("unknown function {name}"))
        }
        fn eval_expr_str(
            &mut self,
            s: &str,
        ) -> Result<super::value::Value, String> {
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
            TestCtx { vars: HashMap::new(), params: Vec::new(), cmd: String::new() }
        }
    }

    impl EvalContext for TestCtx {
        fn get_var(&self, name: &str) -> Option<Value> {
            self.vars.get(name).cloned()
        }
        fn set_local(&mut self, name: &str, value: Value) {
            self.vars.insert(name.into(), value);
        }
        fn set_global(&mut self, name: &str, value: Value) {
            self.vars.insert(name.into(), value);
        }
        fn positional_params(&self) -> &[String] {
            &self.params
        }
        fn current_cmd_name(&self) -> &str {
            &self.cmd
        }
        fn call_fn(&mut self, _: &str, _: Vec<Value>) -> Result<Value, String> {
            Err("no fns".into())
        }
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
    fn dollar_brace_var() {
        let mut ctx = TestCtx::new();
        ctx.vars.insert("LOGFILE".into(), Value::Str("/tmp/log".into()));
        assert_eq!(exp("${LOGFILE}", &mut ctx), "/tmp/log");
    }

    #[test]
    fn double_dollar_escape() {
        let mut ctx = TestCtx::new();
        assert_eq!(exp("$$foo", &mut ctx), "$foo");
        assert_eq!(exp("a$$b", &mut ctx), "a$b");
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
    fn last_param() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(exp("{L}", &mut ctx), "c");
        assert_eq!(exp("%L", &mut ctx), "c");
    }

    #[test]
    fn last_param_empty() {
        let mut ctx = TestCtx::new();
        assert_eq!(exp("{L}", &mut ctx), "");
    }

    #[test]
    fn all_but_last() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(exp("{-L}", &mut ctx), "a b");
        assert_eq!(exp("%-L", &mut ctx), "a b");
    }

    #[test]
    fn all_but_last_single_param() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["only".into()];
        assert_eq!(exp("{-L}", &mut ctx), "");
        assert_eq!(exp("%-L", &mut ctx), "");
    }

    #[test]
    fn all_but_first_n() {
        let mut ctx = TestCtx::new();
        ctx.params = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        assert_eq!(exp("{-1}", &mut ctx), "b c d");
        assert_eq!(exp("%-1", &mut ctx), "b c d");
        assert_eq!(exp("{-2}", &mut ctx), "c d");
        assert_eq!(exp("%-2", &mut ctx), "c d");
        assert_eq!(exp("{-3}", &mut ctx), "d");
    }

    #[test]
    fn last_param_with_default() {
        let mut ctx = TestCtx::new();
        // No params — use default.
        assert_eq!(exp("{L-@}", &mut ctx), "@");
        // With params — last param.
        ctx.params = vec!["foo".into()];
        assert_eq!(exp("{L-@}", &mut ctx), "foo");
    }

    #[test]
    fn param_with_numeric_default() {
        let mut ctx = TestCtx::new();
        // {2-23}: param 2 defaulting to "23"
        ctx.params = vec!["world".into()];
        assert_eq!(exp("{2-23}", &mut ctx), "23");
        ctx.params = vec!["world".into(), "4000".into()];
        assert_eq!(exp("{2-23}", &mut ctx), "4000");
    }

    #[test]
    fn param_with_string_default() {
        let mut ctx = TestCtx::new();
        // {1-x}: param 1 defaulting to "x"
        assert_eq!(exp("{1-x}", &mut ctx), "x");
        ctx.params = vec!["hello".into()];
        assert_eq!(exp("{1-x}", &mut ctx), "hello");
    }

    #[test]
    fn var_with_default() {
        let mut ctx = TestCtx::new();
        // {opt_a-/abort}: variable with default
        assert_eq!(exp("%{opt_a-/abort}", &mut ctx), "/abort");
        ctx.vars.insert("opt_a".into(), Value::Str("/myabort".into()));
        assert_eq!(exp("%{opt_a-/abort}", &mut ctx), "/myabort");
    }

    #[test]
    fn var_with_default_uses_var() {
        // {_file-${LOGFILE}}: variable with default that is itself a variable.
        let mut ctx = TestCtx::new();
        ctx.vars.insert("LOGFILE".into(), Value::Str("/tmp/tf.log".into()));
        // _file not set → use default which expands ${LOGFILE}
        assert_eq!(exp("%{_file-${LOGFILE}}", &mut ctx), "/tmp/tf.log");
        // _file set → use its value
        ctx.vars.insert("_file".into(), Value::Str("/my/file".into()));
        assert_eq!(exp("%{_file-${LOGFILE}}", &mut ctx), "/my/file");
    }

    #[test]
    fn all_params_or_default() {
        let mut ctx = TestCtx::new();
        // No params → use default.
        assert_eq!(exp("{*-@}", &mut ctx), "@");
        ctx.params = vec!["x".into(), "y".into()];
        assert_eq!(exp("{*-@}", &mut ctx), "x y");
    }

    #[test]
    fn require_macro_simulation() {
        // Simulate: /require alias.tf → body is /load %{-L} %{L}
        // Params = ["alias.tf"]
        let mut ctx = TestCtx::new();
        ctx.params = vec!["alias.tf".into()];
        let result = exp("/load %{-L} %{L}", &mut ctx);
        assert_eq!(result, "/load  alias.tf");
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

    #[test]
    fn nested_brace_in_default() {
        // %{-L-%{qdef_prefix-:|}} — all-but-last or value of qdef_prefix
        // (defaulting to ":|")
        let mut ctx = TestCtx::new();
        ctx.vars.insert("qdef_prefix".into(), Value::Str(">>".into()));
        ctx.params = vec!["only".into()];
        // Single param → no all-but-last → expand default %{qdef_prefix-:|}
        assert_eq!(exp("%{-L-%{qdef_prefix-:|}}", &mut ctx), ">>");
    }

    #[test]
    fn double_at_indirect_expansion() {
        let mut ctx = TestCtx::new();
        // %ptr = "target", %target = "hello"
        ctx.vars.insert("ptr".into(),    Value::Str("target".into()));
        ctx.vars.insert("target".into(), Value::Str("hello".into()));
        // @@ptr → value of %ptr ("target") → value of %target ("hello")
        assert_eq!(exp("@@ptr", &mut ctx), "hello");
    }

    #[test]
    fn double_at_missing_inner() {
        let mut ctx = TestCtx::new();
        // %ptr = "nosuch" — second dereference finds nothing → empty string
        ctx.vars.insert("ptr".into(), Value::Str("nosuch".into()));
        assert_eq!(exp("@@ptr", &mut ctx), "");
    }

    #[test]
    fn percent_paren_inline_expr() {
        let mut ctx = TestCtx::new();
        // %(3 + 4) should evaluate to "7"
        assert_eq!(exp("%(3 + 4)", &mut ctx), "7");
    }

    #[test]
    fn percent_paren_with_var() {
        let mut ctx = TestCtx::new();
        ctx.vars.insert("x".into(), Value::Int(10));
        assert_eq!(exp("result=%(x * 2)", &mut ctx), "result=20");
    }
}
