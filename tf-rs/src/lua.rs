//! Optional Lua 5.4 scripting via the `mlua` crate.
//!
//! Enabled with the `lua` Cargo feature:
//! ```text
//! cargo build --features lua
//! cargo test  --features lua
//! ```
//!
//! Corresponds to `lua.c` in the C source.
//!
//! # Commands
//!
//! | TF command        | Rust method                          |
//! |-------------------|--------------------------------------|
//! | `/loadlua path`   | [`LuaEngine::load_file`]             |
//! | `/calllua fn …`   | [`LuaEngine::call_func`]             |
//! | `/purgelua`       | drop the [`LuaEngine`]               |
//!
//! # Lua API
//!
//! The following global functions are pre-registered in every Lua state:
//!
//! | Lua function                     | Effect                              |
//! |----------------------------------|-------------------------------------|
//! | `tf_getvar(name)`                | Read a TF variable → string or nil  |
//! | `tf_setvar(name, value)`         | Write a TF variable → bool          |
//! | `tf_unsetvar(name)`              | Remove a TF variable → bool         |
//! | `tf_eval(command)`               | Execute a TF script command         |
//! | `tf_send(text [,world [,flags]])` | Send text to a world               |

#[cfg(feature = "lua")]
pub use lua_impl::{LuaCommand, LuaEngine};

#[cfg(feature = "lua")]
mod lua_impl {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use mlua::prelude::*;

    use crate::var::VarStore;

    // ── LuaCommand ────────────────────────────────────────────────────────

    /// Command produced by a Lua API call and sent back to the event loop.
    #[derive(Debug, Clone)]
    pub enum LuaCommand {
        /// `tf_send(text, world, flags)` — send a line to a named world.
        Send {
            text: String,
            world: Option<String>,
            /// Future use; currently unused.
            flags: Option<String>,
        },
        /// `tf_eval(command)` — execute a TF script string.
        Eval { script: String },
    }

    // ── LuaEngine ─────────────────────────────────────────────────────────

    /// A Lua 5.4 interpreter instance with TF's API pre-registered.
    ///
    /// Create once with [`LuaEngine::new`], call [`LuaEngine::load_file`]
    /// to source `.lua` scripts, and [`LuaEngine::call_func`] to invoke
    /// functions from TF triggers/macros.  Drop to close (mirrors
    /// `/purgelua`).
    pub struct LuaEngine {
        lua: Lua,
    }

    impl LuaEngine {
        /// Create a new Lua interpreter and register the TF API.
        ///
        /// `vars` — shared mutable TF variable store, used by `tf_getvar` /
        /// `tf_setvar` / `tf_unsetvar`.
        ///
        /// `cmd_tx` — channel through which `tf_send` and `tf_eval` deliver
        /// their work to the event loop.
        pub fn new(
            vars: Arc<Mutex<VarStore>>,
            cmd_tx: std::sync::mpsc::SyncSender<LuaCommand>,
        ) -> LuaResult<Self> {
            let lua = Lua::new();

            Self::register_api(&lua, vars, cmd_tx)?;

            Ok(Self { lua })
        }

        // ── TF API registration ───────────────────────────────────────────

        fn register_api(
            lua: &Lua,
            vars: Arc<Mutex<VarStore>>,
            cmd_tx: std::sync::mpsc::SyncSender<LuaCommand>,
        ) -> LuaResult<()> {
            let globals = lua.globals();

            // tf_getvar(name) → string | nil
            {
                let vars = Arc::clone(&vars);
                globals.set(
                    "tf_getvar",
                    lua.create_function(move |_, name: String| {
                        if name.is_empty() {
                            return Err(LuaError::RuntimeError(
                                "bad argument #1 to 'tf_getvar' (name must not be empty)".into(),
                            ));
                        }
                        let v = vars.lock().unwrap();
                        Ok(v.get(&name).map(str::to_owned))
                    })?,
                )?;
            }

            // tf_setvar(name, value) → bool
            //   value: string | integer | float | boolean | nil (nil = unset)
            {
                let vars = Arc::clone(&vars);
                globals.set(
                    "tf_setvar",
                    lua.create_function(move |_, (name, value): (String, LuaValue)| {
                        if name.is_empty() {
                            return Err(LuaError::RuntimeError(
                                "bad argument #1 to 'tf_setvar' (name must not be empty)".into(),
                            ));
                        }
                        let mut v = vars.lock().unwrap();
                        match value {
                            LuaValue::Nil => {
                                v.unset(&name);
                            }
                            LuaValue::Boolean(b) => {
                                v.set(name, if b { "1" } else { "0" });
                            }
                            LuaValue::Integer(i) => {
                                v.set(name, i.to_string());
                            }
                            LuaValue::Number(f) => {
                                v.set(name, f.to_string());
                            }
                            LuaValue::String(s) => {
                                v.set(name, s.to_str()?.to_owned());
                            }
                            other => {
                                return Err(LuaError::RuntimeError(format!(
                                    "bad argument #2 to 'tf_setvar' (unsupported type: {})",
                                    other.type_name()
                                )));
                            }
                        }
                        Ok(true)
                    })?,
                )?;
            }

            // tf_unsetvar(name) → bool
            {
                let vars = Arc::clone(&vars);
                globals.set(
                    "tf_unsetvar",
                    lua.create_function(move |_, name: String| {
                        if name.is_empty() {
                            return Err(LuaError::RuntimeError(
                                "bad argument #1 to 'tf_unsetvar' (name must not be empty)".into(),
                            ));
                        }
                        let mut v = vars.lock().unwrap();
                        Ok(v.unset(&name))
                    })?,
                )?;
            }

            // tf_eval(command) — execute a TF script, no return value
            {
                let tx = cmd_tx.clone();
                globals.set(
                    "tf_eval",
                    lua.create_function(move |_, script: String| {
                        let _ = tx.try_send(LuaCommand::Eval { script });
                        Ok(())
                    })?,
                )?;
            }

            // tf_send(text [, world [, flags]]) → integer (1 = ok)
            {
                let tx = cmd_tx.clone();
                globals.set(
                    "tf_send",
                    lua.create_function(
                        move |_, (text, world, flags): (String, Option<String>, Option<String>)| {
                            let world = world.filter(|s| !s.is_empty());
                            let _ = tx.try_send(LuaCommand::Send { text, world, flags });
                            Ok(1i64)
                        },
                    )?,
                )?;
            }

            Ok(())
        }

        // ── File loading ──────────────────────────────────────────────────

        /// Load and execute a Lua source file (mirrors `/loadlua path`).
        pub fn load_file(&self, path: &Path) -> LuaResult<()> {
            self.lua.load(path).exec()
        }

        // ── Function calls ────────────────────────────────────────────────

        /// Call a named Lua function with zero or more string arguments.
        ///
        /// The return type `R` must implement [`FromLuaMulti`]; use
        /// [`LuaValue`] to accept any Lua value, or a concrete Rust type
        /// (e.g. `String`, `bool`, `i64`) to coerce automatically.
        ///
        /// Mirrors `/calllua function_name [args…]`.
        pub fn call_func<R: FromLuaMulti>(
            &self,
            name: &str,
            args: impl IntoIterator<Item = impl AsRef<str>>,
        ) -> LuaResult<R> {
            let func: LuaFunction = self.lua.globals().get(name)?;
            let lua_args: LuaMultiValue = args
                .into_iter()
                .map(|a| Ok(LuaValue::String(self.lua.create_string(a.as_ref())?)))
                .collect::<LuaResult<Vec<_>>>()?
                .into();
            func.call::<R>(lua_args)
        }

        /// Call a named Lua function passing a string body, a per-character
        /// attribute array, and a line-level attribute integer — matching the
        /// three-argument convention of `/calllua` when text with attributes
        /// is passed.
        pub fn call_func_with_attrs<R: FromLuaMulti>(
            &self,
            name: &str,
            body: &str,
            char_attrs: &[u32],
            line_attr: u32,
        ) -> LuaResult<R> {
            let func: LuaFunction = self.lua.globals().get(name)?;

            // Build the per-char attribute table.
            let tbl = self.lua.create_table()?;
            for (i, &attr) in char_attrs.iter().enumerate() {
                tbl.raw_set(i as i64, attr)?;
            }

            func.call::<R>((body, tbl, line_attr))
        }

        // ── Eval ──────────────────────────────────────────────────────────

        /// Execute an arbitrary Lua chunk string.
        pub fn exec(&self, chunk: &str) -> LuaResult<()> {
            self.lua.load(chunk).exec()
        }

        /// Evaluate a Lua expression string and return its value.
        ///
        /// The return type `R` must implement [`FromLuaMulti`]; use
        /// [`LuaValue`] to accept any Lua value, or a concrete Rust type
        /// (e.g. `String`, `bool`, `i64`) to coerce automatically.
        pub fn eval<R: FromLuaMulti>(&self, expr: &str) -> LuaResult<R> {
            self.lua.load(expr).eval()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "lua"))]
mod tests {
    use super::lua_impl::*;
    use crate::var::VarStore;
    use std::sync::{Arc, Mutex};

    fn make_engine() -> (LuaEngine, std::sync::mpsc::Receiver<LuaCommand>) {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let engine = LuaEngine::new(vars, tx).unwrap();
        (engine, rx)
    }

    fn make_engine_with_vars(
        vars: Arc<Mutex<VarStore>>,
    ) -> (LuaEngine, std::sync::mpsc::Receiver<LuaCommand>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let engine = LuaEngine::new(vars, tx).unwrap();
        (engine, rx)
    }

    // ── tf_getvar / tf_setvar / tf_unsetvar ───────────────────────────────

    #[test]
    fn getvar_returns_nil_for_missing() {
        let (eng, _rx) = make_engine();
        let v: mlua::Value = eng.eval("tf_getvar('nosuchvar')").unwrap();
        assert!(v.is_nil());
    }

    #[test]
    fn setvar_then_getvar_roundtrip() {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let (eng, _rx) = make_engine_with_vars(Arc::clone(&vars));

        eng.exec("tf_setvar('greeting', 'hello')").unwrap();
        assert_eq!(vars.lock().unwrap().get("greeting"), Some("hello"));

        let v: String = eng.eval("tf_getvar('greeting')").unwrap();
        assert_eq!(v, "hello");
    }

    #[test]
    fn setvar_integer() {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let (eng, _rx) = make_engine_with_vars(Arc::clone(&vars));
        eng.exec("tf_setvar('hp', 100)").unwrap();
        assert_eq!(vars.lock().unwrap().get("hp"), Some("100"));
    }

    #[test]
    fn setvar_boolean_true() {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let (eng, _rx) = make_engine_with_vars(Arc::clone(&vars));
        eng.exec("tf_setvar('flag', true)").unwrap();
        assert_eq!(vars.lock().unwrap().get("flag"), Some("1"));
    }

    #[test]
    fn setvar_nil_unsets() {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        vars.lock().unwrap().set("temp", "42");
        let (eng, _rx) = make_engine_with_vars(Arc::clone(&vars));
        eng.exec("tf_setvar('temp', nil)").unwrap();
        assert!(vars.lock().unwrap().get("temp").is_none());
    }

    #[test]
    fn unsetvar_removes_key() {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        vars.lock().unwrap().set("x", "99");
        let (eng, _rx) = make_engine_with_vars(Arc::clone(&vars));
        let result: bool = eng.eval("tf_unsetvar('x')").unwrap();
        assert!(result);
        assert!(vars.lock().unwrap().get("x").is_none());
    }

    #[test]
    fn unsetvar_missing_returns_false() {
        let (eng, _rx) = make_engine();
        let result: bool = eng.eval("tf_unsetvar('ghost')").unwrap();
        assert!(!result);
    }

    // ── tf_eval ───────────────────────────────────────────────────────────

    #[test]
    fn tf_eval_sends_command() {
        let (eng, rx) = make_engine();
        eng.exec("tf_eval('/echo hello')").unwrap();
        match rx.try_recv().unwrap() {
            LuaCommand::Eval { script } => assert_eq!(script, "/echo hello"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── tf_send ───────────────────────────────────────────────────────────

    #[test]
    fn tf_send_without_world() {
        let (eng, rx) = make_engine();
        eng.exec("tf_send('look')").unwrap();
        match rx.try_recv().unwrap() {
            LuaCommand::Send { text, world, .. } => {
                assert_eq!(text, "look");
                assert!(world.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tf_send_with_world() {
        let (eng, rx) = make_engine();
        eng.exec("tf_send('north', 'mud1')").unwrap();
        match rx.try_recv().unwrap() {
            LuaCommand::Send { text, world, .. } => {
                assert_eq!(text, "north");
                assert_eq!(world.as_deref(), Some("mud1"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── call_func ─────────────────────────────────────────────────────────

    #[test]
    fn call_func_no_args() {
        let (eng, _rx) = make_engine();
        eng.exec("function ping() return 'pong' end").unwrap();
        let v: String = eng.call_func("ping", [] as [&str; 0]).unwrap();
        assert_eq!(v, "pong");
    }

    #[test]
    fn call_func_with_string_args() {
        let (eng, _rx) = make_engine();
        eng.exec("function greet(name) return 'hello ' .. name end").unwrap();
        let v: String = eng.call_func("greet", ["world"]).unwrap();
        assert_eq!(v, "hello world");
    }

    #[test]
    fn call_func_missing_returns_error() {
        let (eng, _rx) = make_engine();
        let err = eng.call_func::<()>("no_such_fn", [] as [&str; 0]);
        assert!(err.is_err());
    }

    #[test]
    fn call_func_with_attrs() {
        let (eng, _rx) = make_engine();
        eng.exec(
            "function on_line(body, attrs, line_attr) return body .. '!' end",
        )
        .unwrap();
        let v: String = eng
            .call_func_with_attrs("on_line", "hello", &[0, 0, 0, 0, 0], 0)
            .unwrap();
        assert_eq!(v, "hello!");
    }

    // ── error handling ────────────────────────────────────────────────────

    #[test]
    fn lua_runtime_error_propagates() {
        let (eng, _rx) = make_engine();
        let err = eng.exec("error('boom')");
        assert!(err.is_err());
    }

    #[test]
    fn empty_name_is_an_error() {
        let (eng, _rx) = make_engine();
        assert!(eng.exec("tf_getvar('')").is_err());
        assert!(eng.exec("tf_setvar('', 'v')").is_err());
        assert!(eng.exec("tf_unsetvar('')").is_err());
    }

    // ── load_file ─────────────────────────────────────────────────────────

    #[test]
    fn load_file_executes_script() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "loaded = true").unwrap();
        let (eng, _rx) = make_engine();
        eng.load_file(f.path()).unwrap();
        let v: bool = eng.eval("loaded").unwrap();
        assert!(v);
    }
}
