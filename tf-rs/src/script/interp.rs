//! TF script interpreter.
//!
//! The [`Interpreter`] holds the variable stack and world state and
//! executes parsed [`Stmt`] trees.  It implements [`EvalContext`] so the
//! expression evaluator can call back into it for variable lookups and
//! function calls.

use std::collections::HashMap;

use super::{
    builtins::call_builtin,
    expand::expand,
    expr::{eval_str, EvalContext},
    stmt::{parse_script, Stmt},
    value::Value,
};

// ── ControlFlow ───────────────────────────────────────────────────────────────

/// Non-error control-flow signals that can unwind the call stack.
#[derive(Debug)]
pub enum ControlFlow {
    Break,
    Return(Value),
}

// ── Variable scope frame ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Frame {
    locals: HashMap<String, Value>,
    params: Vec<String>,
    cmd_name: String,
}

// ── Interpreter ───────────────────────────────────────────────────────────────

/// The TF script interpreter.
pub struct Interpreter {
    /// Global variable store.
    globals: HashMap<String, Value>,
    /// Local variable stack (innermost frame last).
    frames: Vec<Frame>,
    /// User-defined macros: name → script source.
    macros: HashMap<String, String>,
    /// Lines of output produced by `/echo`.
    pub output: Vec<String>,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            globals: HashMap::new(),
            frames: Vec::new(),
            macros: HashMap::new(),
            output: Vec::new(),
        }
    }

    /// Define a user macro (name → TF script source).
    pub fn define_macro(&mut self, name: impl Into<String>, body: impl Into<String>) {
        self.macros.insert(name.into(), body.into());
    }

    /// Set a global variable.
    pub fn set_global_var(&mut self, name: impl Into<String>, value: Value) {
        self.globals.insert(name.into(), value);
    }

    /// Get a global variable.
    pub fn get_global_var(&self, name: &str) -> Option<&Value> {
        self.globals.get(name)
    }

    // ── Execution ─────────────────────────────────────────────────────────────

    /// Execute a TF script string, returning the last value or a
    /// control-flow signal.
    pub fn exec_script(&mut self, src: &str) -> Result<Value, String> {
        let stmts = parse_script(src).map_err(|e| format!("parse error: {e}"))?;
        match self.exec_block(&stmts)? {
            Some(ControlFlow::Return(v)) => Ok(v),
            Some(ControlFlow::Break) => Ok(Value::default()),
            None => Ok(Value::default()),
        }
    }

    /// Execute a pre-parsed block of statements.
    ///
    /// Returns `Ok(Some(ControlFlow))` when a `/break` or `/return` is hit.
    pub fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Option<ControlFlow>, String> {
        for stmt in stmts {
            if let Some(cf) = self.exec_stmt(stmt)? {
                return Ok(Some(cf));
            }
        }
        Ok(None)
    }

    /// Execute a single statement.
    pub fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Option<ControlFlow>, String> {
        match stmt {
            Stmt::Raw(text) => {
                let expanded = expand(text, self)?;
                self.output.push(expanded);
                Ok(None)
            }

            Stmt::Echo { text, newline } => {
                let expanded = expand(text, self)?;
                if *newline {
                    self.output.push(expanded);
                } else {
                    // Append to last output line without newline.
                    if let Some(last) = self.output.last_mut() {
                        last.push_str(&expanded);
                    } else {
                        self.output.push(expanded);
                    }
                }
                Ok(None)
            }

            Stmt::Send { text } => {
                let expanded = expand(text, self)?;
                // In a full implementation this would send `expanded` to the
                // active world connection.  For now we record it as output.
                self.output.push(format!("[send] {expanded}"));
                Ok(None)
            }

            Stmt::Set { name, value } => {
                let expanded = expand(value, self)?;
                let val = try_parse_number(&expanded);
                self.set_global(name, val);
                Ok(None)
            }

            Stmt::Let { name, value } => {
                let expanded = expand(value, self)?;
                let val = try_parse_number(&expanded);
                self.set_local(name, val);
                Ok(None)
            }

            Stmt::Unset { name } => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.locals.remove(name);
                }
                self.globals.remove(name);
                Ok(None)
            }

            Stmt::Expr { src } => {
                let expanded = expand(src, self)?;
                eval_str(&expanded, self)?;
                Ok(None)
            }

            Stmt::Return { value } => {
                let v = match value {
                    None => Value::default(),
                    Some(src) => {
                        let expanded = expand(src, self)?;
                        eval_str(&expanded, self)?
                    }
                };
                Ok(Some(ControlFlow::Return(v)))
            }

            Stmt::Break => Ok(Some(ControlFlow::Break)),

            Stmt::If { cond, then_block, else_block } => {
                let expanded = expand(cond, self)?;
                let val = eval_str(&expanded, self)?;
                let block = if val.as_bool() { then_block } else { else_block };
                self.exec_block(block)
            }

            Stmt::While { cond, body } => {
                loop {
                    let expanded = expand(cond, self)?;
                    let val = eval_str(&expanded, self)?;
                    if !val.as_bool() { break; }
                    match self.exec_block(body)? {
                        Some(ControlFlow::Break) => break,
                        Some(cf @ ControlFlow::Return(_)) => return Ok(Some(cf)),
                        None => {}
                    }
                }
                Ok(None)
            }

            Stmt::For { var, start, end, body } => {
                // TF range loop: iterate var from start to end (inclusive).
                let start_str = expand(start, self)?;
                let end_str = expand(end, self)?;
                let start_val: i64 = start_str.trim().parse()
                    .map_err(|_| format!("invalid /for start value: {start_str}"))?;
                let end_val: i64 = end_str.trim().parse()
                    .map_err(|_| format!("invalid /for end value: {end_str}"))?;
                for i in start_val..=end_val {
                    self.set_local(var, Value::Int(i));
                    match self.exec_block(body)? {
                        Some(ControlFlow::Break) => break,
                        Some(cf @ ControlFlow::Return(_)) => return Ok(Some(cf)),
                        None => {}
                    }
                }
                Ok(None)
            }

            Stmt::AddWorld { .. } => {
                // Forward to config layer in a full implementation.
                Ok(None)
            }

            Stmt::Command { name, args } => {
                // Try user-defined macro.
                let src = self.macros.get(name).cloned();
                if let Some(body) = src {
                    let expanded_args = expand(args, self)?;
                    let params: Vec<String> = expanded_args
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect();
                    return self.invoke_macro(&body, name, params);
                }
                // Unknown command — silently ignore (like the C source).
                Ok(None)
            }
        }
    }

    /// Invoke a macro body with positional parameters.
    fn invoke_macro(
        &mut self,
        body: &str,
        name: &str,
        params: Vec<String>,
    ) -> Result<Option<ControlFlow>, String> {
        self.frames.push(Frame {
            locals: HashMap::new(),
            params,
            cmd_name: name.to_owned(),
        });
        let stmts = parse_script(body)?;
        let result = self.exec_block(&stmts);
        self.frames.pop();
        result
    }
}

// ── EvalContext impl ──────────────────────────────────────────────────────────

impl EvalContext for Interpreter {
    fn get_var(&self, name: &str) -> Option<Value> {
        // Local scope first (innermost frame)
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.locals.get(name) {
                return Some(v.clone());
            }
        }
        self.globals.get(name).cloned()
    }

    fn set_local(&mut self, name: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.insert(name.to_owned(), value);
        } else {
            self.globals.insert(name.to_owned(), value);
        }
    }

    fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(name.to_owned(), value);
    }

    fn positional_params(&self) -> &[String] {
        self.frames.last().map(|f| f.params.as_slice()).unwrap_or(&[])
    }

    fn current_cmd_name(&self) -> &str {
        self.frames.last().map(|f| f.cmd_name.as_str()).unwrap_or("")
    }

    fn call_fn(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        // 1. Try built-ins
        if let Some(result) = call_builtin(name, args.clone()) {
            return result;
        }
        // 2. Try user-defined macros
        let src = self.macros.get(name).cloned();
        if let Some(body) = src {
            let params: Vec<String> = args.iter().map(|v| v.to_string()).collect();
            self.invoke_macro(&body, name, params)?;
            // Return value is delivered via ControlFlow::Return; extract from output.
            return Ok(Value::default());
        }
        Err(format!("unknown function: {name}"))
    }

    fn eval_expr_str(&mut self, s: &str) -> Result<Value, String> {
        eval_str(s, self)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a string as an integer or float if possible; otherwise keep as Str.
fn try_parse_number(s: &str) -> Value {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        Value::Int(n)
    } else if let Ok(x) = t.parse::<f64>() {
        Value::Float(x)
    } else {
        Value::Str(s.to_owned())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Interpreter {
        let mut interp = Interpreter::new();
        interp.exec_script(src).expect("exec failed");
        interp
    }

    fn output(src: &str) -> Vec<String> {
        run(src).output
    }

    #[test]
    fn echo_basic() {
        assert_eq!(output("/echo Hello"), vec!["Hello"]);
    }

    #[test]
    fn echo_var_expand() {
        let mut interp = Interpreter::new();
        interp.set_global_var("name", Value::Str("Alice".into()));
        interp.exec_script("/echo Hello, %{name}!").unwrap();
        assert_eq!(interp.output, vec!["Hello, Alice!"]);
    }

    #[test]
    fn set_and_get_global() {
        let mut interp = Interpreter::new();
        interp.exec_script("/set x=10").unwrap();
        assert_eq!(interp.get_global_var("x"), Some(&Value::Int(10)));
    }

    #[test]
    fn if_true() {
        let src = "/set x=5\n/if (x > 3)\n/echo yes\n/endif";
        assert_eq!(output(src), vec!["yes"]);
    }

    #[test]
    fn if_false() {
        let src = "/set x=1\n/if (x > 3)\n/echo yes\n/else\n/echo no\n/endif";
        assert_eq!(output(src), vec!["no"]);
    }

    #[test]
    fn while_loop() {
        // Use $[i+1] so the increment actually evaluates the expression.
        let src = "/set i=0\n/while (i < 3)\n/echo loop\n/set i=$[i+1]\n/done";
        let out = output(src);
        assert_eq!(out, vec!["loop", "loop", "loop"]);
    }

    #[test]
    fn while_break() {
        let src = "/set i=0\n/while (1)\n/set i=%{i}\n/break\n/done";
        let out = output(src);
        // The break exits immediately — no echo, just no infinite loop.
        assert!(out.is_empty());
    }

    #[test]
    fn return_from_script() {
        let mut interp = Interpreter::new();
        let v = interp.exec_script("/return 42").unwrap();
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn expr_in_echo() {
        let src = "/echo result=$[3 * 7]";
        assert_eq!(output(src), vec!["result=21"]);
    }

    #[test]
    fn macro_call() {
        let mut interp = Interpreter::new();
        interp.define_macro("greet", "/echo Hello, {1}!");
        interp.exec_script("/greet World").unwrap();
        assert_eq!(interp.output, vec!["Hello, World!"]);
    }

    #[test]
    fn builtin_strlen_via_expr() {
        let src = "/echo $[strlen(\"hello\")]";
        assert_eq!(output(src), vec!["5"]);
    }

    #[test]
    fn for_range_loop() {
        let src = "/for i 1 3 /echo %i";
        assert_eq!(output(src), vec!["1", "2", "3"]);
    }

    #[test]
    fn for_range_sets_var() {
        let src = "/for n 10 12 /set last=%n\n/echo %last";
        let out = output(src);
        assert_eq!(out, vec!["12"]);
    }

    #[test]
    fn local_scope_isolation() {
        let mut interp = Interpreter::new();
        interp.set_global_var("x", Value::Int(99));
        interp.define_macro("setx", "/let x=42");
        interp.exec_script("/setx").unwrap();
        // Global x should be unchanged after macro's /let
        assert_eq!(interp.get_global_var("x"), Some(&Value::Int(99)));
    }
}
