//! TF script interpreter.
//!
//! The [`Interpreter`] holds the variable stack and world state and
//! executes parsed [`Stmt`] trees.  It implements [`EvalContext`] so the
//! expression evaluator can call back into it for variable lookups and
//! function calls.

use std::collections::HashMap;
use std::sync::Arc;

use super::{
    builtins::call_builtin,
    expand::expand,
    expr::{eval_str, EvalContext},
    stmt::{parse_script, Stmt},
    value::Value,
};

// ── ScriptAction ──────────────────────────────────────────────────────────────

/// A side-effect queued by the interpreter that requires event-loop involvement.
///
/// The interpreter is synchronous; these actions are drained by [`EventLoop`]
/// after each [`Interpreter::exec_script`] call (or, for `/load`/`/eval`,
/// processed inline by the interpreter's file loader).
#[derive(Debug, Clone)]
pub enum ScriptAction {
    /// Send text to a world (`None` = active world).
    SendToWorld { text: String, world: Option<String> },
    /// Open a connection to a named world (or the default world if empty).
    Connect { name: String },
    /// Close a connection.
    Disconnect { name: String },
    /// Add / update a world definition.
    AddWorld(crate::world::World),
    /// Switch the active world.
    SwitchWorld { name: String },
    /// Terminate the event loop.
    Quit,
}

// ── File loader callback ──────────────────────────────────────────────────────

/// A callback that resolves a path string (after variable expansion) to the
/// file's contents.  The loader is responsible for tilde expansion and file
/// system access.
pub type FileLoader = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

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
    /// Side-effects queued for the event loop.
    pub actions: Vec<ScriptAction>,
    /// Optional callback for `/load` and `/eval /load …`.
    pub file_loader: Option<FileLoader>,
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
            actions: Vec::new(),
            file_loader: None,
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

    /// Drain and return all queued [`ScriptAction`]s.
    pub fn take_actions(&mut self) -> Vec<ScriptAction> {
        std::mem::take(&mut self.actions)
    }

    // ── Execution ─────────────────────────────────────────────────────────────

    /// Execute a TF script string.
    pub fn exec_script(&mut self, src: &str) -> Result<Value, String> {
        let stmts = parse_script(src).map_err(|e| format!("parse error: {e}"))?;
        match self.exec_block(&stmts)? {
            Some(ControlFlow::Return(v)) => Ok(v),
            Some(ControlFlow::Break) => Ok(Value::default()),
            None => Ok(Value::default()),
        }
    }

    /// Execute a pre-parsed block of statements.
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
                } else if let Some(last) = self.output.last_mut() {
                    last.push_str(&expanded);
                } else {
                    self.output.push(expanded);
                }
                Ok(None)
            }

            Stmt::Send { text } => {
                let expanded = expand(text, self)?;
                self.actions.push(ScriptAction::SendToWorld {
                    text: expanded,
                    world: None,
                });
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
                    if !val.as_bool() {
                        break;
                    }
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
                let start_val: i64 = start_str
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid /for start value: {start_str}"))?;
                let end_val: i64 = end_str
                    .trim()
                    .parse()
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

            Stmt::AddWorld { args } => {
                match crate::config::parse_world_from_tokens(args) {
                    Ok(world) => self.actions.push(ScriptAction::AddWorld(world)),
                    Err(e) => self.output.push(format!("% {e}")),
                }
                Ok(None)
            }

            Stmt::Command { name, args } => {
                // Try user-defined macro first.
                let src = self.macros.get(name.as_str()).cloned();
                if let Some(body) = src {
                    let expanded_args = expand(args, self)?;
                    let params: Vec<String> =
                        expanded_args.split_whitespace().map(str::to_owned).collect();
                    return self.invoke_macro(&body, name, params);
                }

                // Built-in command dispatch.
                self.exec_builtin(name, args)
            }
        }
    }

    // ── Built-in command dispatch ──────────────────────────────────────────────

    fn exec_builtin(&mut self, name: &str, args: &str) -> Result<Option<ControlFlow>, String> {
        match name {
            // ── Lifecycle ──────────────────────────────────────────────────────
            "quit" | "exit" => {
                self.actions.push(ScriptAction::Quit);
                Ok(Some(ControlFlow::Return(Value::default())))
            }

            // ── Macro definition ───────────────────────────────────────────────
            "def" => {
                // /def [-flags…] name = body
                exec_def(args, &mut self.macros);
                Ok(None)
            }

            // ── File loading ───────────────────────────────────────────────────
            "load" => {
                // /load [-q] [-L <dir>] <file>
                let (quiet, path_expr) = parse_load_flags(args);
                let path = expand(path_expr, self)?;
                let path = path.trim().to_owned();
                let loader = self.file_loader.clone();
                if let Some(loader) = loader {
                    match loader(&path) {
                        Ok(src) => {
                            let stmts = parse_script(&src)
                                .map_err(|e| format!("{path}: parse error: {e}"))?;
                            self.exec_block(&stmts)?;
                        }
                        Err(e) if !quiet => {
                            self.output.push(format!("% {e}"));
                        }
                        Err(_) => {} // quiet mode — suppress "not found"
                    }
                }
                Ok(None)
            }

            // ── Expression / command evaluation ───────────────────────────────
            "eval" => {
                // /eval <command-or-expression>
                let expanded = expand(args, self)?;
                let trimmed = expanded.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                let stmts =
                    parse_script(trimmed).map_err(|e| format!("/eval: parse error: {e}"))?;
                self.exec_block(&stmts)
            }

            // ── World / connection management ──────────────────────────────────
            "addworld" => {
                let tokens: Vec<String> =
                    tokenise_addworld(&expand(args, self)?);
                match crate::config::parse_world_from_tokens(&tokens) {
                    Ok(world) => self.actions.push(ScriptAction::AddWorld(world)),
                    Err(e) => self.output.push(format!("% {e}")),
                }
                Ok(None)
            }

            "connect" | "fg" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::Connect { name: expanded.trim().to_owned() });
                Ok(None)
            }

            "disconnect" | "dc" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::Disconnect {
                    name: expanded.trim().to_owned(),
                });
                Ok(None)
            }

            "world" => {
                let expanded = expand(args, self)?;
                self.actions.push(ScriptAction::SwitchWorld {
                    name: expanded.trim().to_owned(),
                });
                Ok(None)
            }

            // ── Echo / output ──────────────────────────────────────────────────
            "echo" => {
                let (no_nl, rest) =
                    if let Some(s) = args.strip_prefix("-n ").or_else(|| args.strip_prefix("-n\t")) {
                        (true, s)
                    } else {
                        (false, args)
                    };
                let expanded = expand(rest, self)?;
                if no_nl {
                    if let Some(last) = self.output.last_mut() {
                        last.push_str(&expanded);
                    } else {
                        self.output.push(expanded);
                    }
                } else {
                    self.output.push(expanded);
                }
                Ok(None)
            }

            // ── Unknown — silently ignore (like the C source) ──────────────────
            _ => Ok(None),
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
        if let Some(result) = call_builtin(name, args.clone()) {
            return result;
        }
        let src = self.macros.get(name).cloned();
        if let Some(body) = src {
            let params: Vec<String> = args.iter().map(|v| v.to_string()).collect();
            self.invoke_macro(&body, name, params)?;
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

/// Parse `/def [-flags…] name = body` and insert into the macro map.
///
/// Flags are skipped (they control triggers/hooks/priority, which are Phase 13).
/// The only thing we extract is the `name = body` pair.
fn exec_def(raw: &str, macros: &mut HashMap<String, String>) {
    let mut rest = raw.trim();

    // Skip flag tokens (start with `-`).
    while rest.starts_with('-') {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let flag = &rest[1..end]; // flag chars after the `-`
        rest = rest[end..].trim_start();

        // Flags that consume one additional token: -p -g -b -t -h -s -w -n -m -c -T -E
        // If the flag part is a single letter that requires an arg AND no arg was
        // embedded directly in the flag token, consume the next word.
        if flag.len() == 1
            && matches!(
                flag.chars().next(),
                Some('p' | 'g' | 'b' | 't' | 'h' | 's' | 'w' | 'n' | 'm' | 'c' | 'T' | 'E')
            )
        {
            // Only consume next token if the flag had no embedded arg.
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            rest = rest[end..].trim_start();
        }
    }

    // rest = "name = body"  or  "name"  (listing — ignore)
    if let Some(eq) = rest.find('=') {
        let name = rest[..eq].trim();
        let body = rest[eq + 1..].trim();
        if !name.is_empty() {
            macros.insert(name.to_owned(), body.to_owned());
        }
    }
}

/// Extract the `-q` (quiet) flag and remaining path from `/load` args.
fn parse_load_flags(args: &str) -> (bool, &str) {
    let mut rest = args.trim();
    let mut quiet = false;
    loop {
        if let Some(r) = rest.strip_prefix("-q") {
            quiet = true;
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix("-L ").or_else(|| rest.strip_prefix("-L\t")) {
            // -L <dir> — skip the next token (we ignore the dir override here;
            // the caller's variable expansion handles TFLIBDIR).
            let end = r.find(char::is_whitespace).unwrap_or(r.len());
            rest = r[end..].trim_start();
        } else {
            break;
        }
    }
    (quiet, rest)
}

/// Tokenise an `/addworld` argument string (respects double-quoted tokens).
fn tokenise_addworld(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_q = !in_q,
            '\\' if in_q => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
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
        let src = "/set i=0\n/while (i < 3)\n/echo loop\n/set i=$[i+1]\n/done";
        let out = output(src);
        assert_eq!(out, vec!["loop", "loop", "loop"]);
    }

    #[test]
    fn while_break() {
        let src = "/set i=0\n/while (1)\n/set i=%{i}\n/break\n/done";
        let out = output(src);
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
    fn local_scope_isolation() {
        let mut interp = Interpreter::new();
        interp.set_global_var("x", Value::Int(99));
        interp.define_macro("setx", "/let x=42");
        interp.exec_script("/setx").unwrap();
        assert_eq!(interp.get_global_var("x"), Some(&Value::Int(99)));
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
    fn send_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/send go east").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(
            &actions[0],
            ScriptAction::SendToWorld { text, world: None } if text == "go east"
        ));
    }

    #[test]
    fn quit_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/quit").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(actions[0], ScriptAction::Quit));
    }

    #[test]
    fn def_and_call() {
        let mut interp = Interpreter::new();
        interp.exec_script("/def -i greet = /echo Hi, {1}!").unwrap();
        interp.exec_script("/greet World").unwrap();
        assert_eq!(interp.output, vec!["Hi, World!"]);
    }

    #[test]
    fn load_via_file_loader() {
        let body = Arc::new(|_path: &str| Ok("/echo loaded".to_owned()));
        let mut interp = Interpreter::new();
        interp.file_loader = Some(body);
        interp.exec_script("/load somefile.tf").unwrap();
        assert_eq!(interp.output, vec!["loaded"]);
    }

    #[test]
    fn eval_command() {
        let out = output("/eval /echo from eval");
        assert_eq!(out, vec!["from eval"]);
    }

    #[test]
    fn connect_queues_action() {
        let mut interp = Interpreter::new();
        interp.exec_script("/connect mymud").unwrap();
        let actions = interp.take_actions();
        assert!(matches!(&actions[0], ScriptAction::Connect { name } if name == "mymud"));
    }
}
