//! Optional Python scripting via the `pyo3` crate.
//!
//! Enabled with the `python` Cargo feature:
//! ```text
//! cargo build --features python
//! cargo test  --features python
//! ```
//!
//! Corresponds to `tfpython.c` in the C source.
//!
//! # Python `tf` module
//!
//! The following functions are pre-imported as `tf.*` in every session:
//!
//! | Python function               | Effect                                |
//! |-------------------------------|---------------------------------------|
//! | `tf.getvar(name[, default])`  | Read a TF variable → str or default   |
//! | `tf.eval(command)`            | Execute a TF script command           |
//! | `tf.send(text[, world])`      | Send text to a MUD world              |
//! | `tf.out(text)`                | Display text on the TF output stream  |
//! | `tf.err(text)`                | Display text on the TF error stream   |
//! | `tf.world()`                  | Return the active world name          |

#[cfg(feature = "python")]
pub use python_impl::{PythonCommand, PythonEngine};

#[cfg(feature = "python")]
mod python_impl {
    use std::path::Path;
    use std::sync::{Arc, Mutex, OnceLock};

    use pyo3::prelude::*;

    use crate::var::VarStore;

    // ── PythonCommand ─────────────────────────────────────────────────────

    /// Command produced by a Python API call and sent back to the event loop.
    #[derive(Debug, Clone)]
    pub enum PythonCommand {
        /// `tf.send(text[, world])` — queue a line to a MUD world.
        Send { text: String, world: Option<String> },
        /// `tf.eval(command)` — queue a TF script for the event loop.
        Eval { script: String },
        /// `tf.out(text)` — queue a message to the TF output stream.
        Out { text: String },
        /// `tf.err(text)` — queue a message to the TF error stream.
        Err { text: String },
    }

    // ── Shared state accessed by #[pyfunction]s ───────────────────────────

    struct TfState {
        vars: Arc<Mutex<VarStore>>,
        cmd_tx: std::sync::mpsc::SyncSender<PythonCommand>,
        active_world: Arc<Mutex<Option<String>>>,
    }

    static STATE: Mutex<Option<Arc<TfState>>> = Mutex::new(None);
    static PYTHON_INIT: OnceLock<()> = OnceLock::new();

    // ── tf.* Python functions ─────────────────────────────────────────────

    /// `tf.getvar(name[, default])` → str | None
    #[pyfunction]
    #[pyo3(name = "getvar", signature = (name, default = None))]
    fn pytf_getvar(name: &str, default: Option<String>) -> Option<String> {
        let guard = STATE.lock().unwrap();
        let Some(state) = guard.as_ref() else { return default };
        let vars = state.vars.lock().unwrap();
        vars.get(name).map(str::to_owned).or(default)
    }

    /// `tf.eval(command)` — queue a TF script for the event loop.
    #[pyfunction]
    #[pyo3(name = "eval")]
    fn pytf_eval(script: String) -> PyResult<()> {
        // Clone Arc out before releasing STATE lock so send() doesn't hold the lock.
        let state_arc = STATE.lock().unwrap().clone();
        if let Some(state) = state_arc {
            state.cmd_tx.send(PythonCommand::Eval { script })
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("tf channel closed: {e}")))?;
        }
        Ok(())
    }

    /// `tf.send(text[, world])` — queue a line to a MUD world.
    #[pyfunction]
    #[pyo3(name = "send", signature = (text, world = None))]
    fn pytf_send(text: String, world: Option<String>) -> PyResult<()> {
        let state_arc = STATE.lock().unwrap().clone();
        if let Some(state) = state_arc {
            let world = world.filter(|s| !s.is_empty());
            state.cmd_tx.send(PythonCommand::Send { text, world })
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("tf channel closed: {e}")))?;
        }
        Ok(())
    }

    /// `tf.out(text)` — display text on the TF output stream.
    #[pyfunction]
    #[pyo3(name = "out")]
    fn pytf_out(text: String) -> PyResult<()> {
        let state_arc = STATE.lock().unwrap().clone();
        if let Some(state) = state_arc {
            state.cmd_tx.send(PythonCommand::Out { text })
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("tf channel closed: {e}")))?;
        }
        Ok(())
    }

    /// `tf.err(text)` — display text on the TF error stream.
    #[pyfunction]
    #[pyo3(name = "err")]
    fn pytf_err(text: String) -> PyResult<()> {
        let state_arc = STATE.lock().unwrap().clone();
        if let Some(state) = state_arc {
            state.cmd_tx.send(PythonCommand::Err { text })
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("tf channel closed: {e}")))?;
        }
        Ok(())
    }

    /// `tf.world()` → str — return the active world name (or "").
    #[pyfunction]
    #[pyo3(name = "world")]
    fn pytf_world() -> String {
        // Clone the Arc out before releasing the STATE lock to avoid holding
        // two locks simultaneously (STATE + active_world), which would risk a
        // deadlock if another code path acquires them in the opposite order.
        let state_arc = STATE.lock().unwrap().clone();
        state_arc
            .and_then(|s| s.active_world.lock().unwrap().clone())
            .unwrap_or_default()
    }

    // ── tf module registration ────────────────────────────────────────────

    fn register_tf_module(py: Python<'_>) -> PyResult<()> {
        let m = PyModule::new_bound(py, "tf")?;
        m.add_function(wrap_pyfunction!(pytf_getvar, &m)?)?;
        m.add_function(wrap_pyfunction!(pytf_eval, &m)?)?;
        m.add_function(wrap_pyfunction!(pytf_send, &m)?)?;
        m.add_function(wrap_pyfunction!(pytf_out, &m)?)?;
        m.add_function(wrap_pyfunction!(pytf_err, &m)?)?;
        m.add_function(wrap_pyfunction!(pytf_world, &m)?)?;
        // Register as sys.modules["tf"] so `import tf` works.
        let sys = py.import_bound("sys")?;
        sys.getattr("modules")?.set_item("tf", &m)?;
        Ok(())
    }

    /// One-time init script: redirect stdout/stderr through TF's API and
    /// reset `sys.argv`.  Mirrors C's `init_src` in `tfpython.c`.
    const INIT_SRC: &str = "\
import sys, tf

class _TfStream:
    def __init__(self, output):
        self._buf = ''
        self._output = output
    def write(self, s):
        if self._output is None:
            return
        self._buf += s
        while '\\n' in self._buf:
            line, self._buf = self._buf.split('\\n', 1)
            self._output(line)
    def flush(self):
        pass

sys.stdout = _TfStream(None)
sys.stderr = _TfStream(tf.err)
sys.argv = ['tf']
";

    // ── PythonEngine ──────────────────────────────────────────────────────

    /// A Python interpreter session with TF's API pre-imported.
    ///
    /// Create with [`PythonEngine::new`], run Python code with
    /// [`PythonEngine::exec`] / [`PythonEngine::eval_expr`], and drop to
    /// make `tf.*` callbacks become no-ops (mirrors `/killpython`).
    pub struct PythonEngine;

    impl PythonEngine {
        /// Create (or re-attach to) the Python interpreter with fresh TF state.
        ///
        /// The CPython interpreter is initialised at most once per process;
        /// subsequent calls update the shared vars/channel/world without
        /// reinitialising Python.
        pub fn new(
            vars: Arc<Mutex<VarStore>>,
            cmd_tx: std::sync::mpsc::SyncSender<PythonCommand>,
            active_world: Arc<Mutex<Option<String>>>,
        ) -> PyResult<Self> {
            *STATE.lock().unwrap() =
                Some(Arc::new(TfState { vars, cmd_tx, active_world }));

            // Initialise the interpreter exactly once (CPython limitation).
            PYTHON_INIT.get_or_init(pyo3::prepare_freethreaded_python);

            Python::with_gil(|py| {
                register_tf_module(py)?;
                py.run_bound(INIT_SRC, None, None)
            })?;

            Ok(Self)
        }

        // ── Execution ─────────────────────────────────────────────────────

        /// Execute Python statements in the `__main__` namespace.
        ///
        /// Mirrors C's `handle_python_command`.
        pub fn exec(&self, code: &str) -> PyResult<()> {
            Python::with_gil(|py| py.run_bound(code, None, None))
        }

        /// Evaluate a Python expression and return the result.
        ///
        /// Mirrors C's `handle_python_function`.
        pub fn eval_expr(&self, expr: &str) -> PyResult<PyObject> {
            Python::with_gil(|py| {
                py.eval_bound(expr, None, None).map(|v| v.unbind())
            })
        }

        /// Call a named function in `__main__` with a single string argument.
        ///
        /// Mirrors C's `handle_python_call_command`.
        pub fn call_func(&self, func_name: &str, arg: &str) -> PyResult<PyObject> {
            Python::with_gil(|py| {
                py.import_bound("__main__")?
                    .getattr(func_name)?
                    .call1((arg,))
                    .map(|v| v.unbind())
            })
        }

        /// Import or reload a Python module by name.
        ///
        /// Mirrors C's `handle_python_load_command`.
        pub fn load_module(&self, name: &str) -> PyResult<()> {
            let code = format!(
                "from importlib import reload\n\
                 try:\n    reload({name})\n\
                 except (NameError, TypeError):\n    import {name}\n"
            );
            self.exec(&code)
        }

        /// Execute a Python source file in the `__main__` namespace.
        pub fn run_file(&self, path: &Path) -> PyResult<()> {
            let code = std::fs::read_to_string(path).map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyOSError, _>(e.to_string())
            })?;
            self.exec(&code)
        }
    }

    impl Drop for PythonEngine {
        /// Clear shared state so `tf.*` functions become no-ops after drop.
        fn drop(&mut self) {
            *STATE.lock().unwrap() = None;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "python"))]
mod tests {
    use super::python_impl::*;
    use crate::var::VarStore;
    use std::sync::{Arc, Mutex};

    use pyo3::Python;

    // Python tests share a single global interpreter state via STATE.
    // Tests MUST run sequentially to prevent clobbering each other's channel
    // or variable store.  Acquire this mutex at the top of every test.
    static TEST_MX: Mutex<()> = Mutex::new(());

    fn make_engine() -> (PythonEngine, std::sync::mpsc::Receiver<PythonCommand>) {
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let world = Arc::new(Mutex::new(None::<String>));
        let engine = PythonEngine::new(vars, tx, world).unwrap();
        (engine, rx)
    }

    fn make_engine_with(
        vars: Arc<Mutex<VarStore>>,
        world: Arc<Mutex<Option<String>>>,
    ) -> (PythonEngine, std::sync::mpsc::Receiver<PythonCommand>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let engine = PythonEngine::new(vars, tx, world).unwrap();
        (engine, rx)
    }

    // ── exec / eval_expr ──────────────────────────────────────────────────

    #[test]
    fn exec_assigns_and_eval_reads() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        eng.exec("_py_test_exec_x = 123").unwrap();
        let v = Python::with_gil(|py| {
            eng.eval_expr("_py_test_exec_x").unwrap().extract::<i64>(py).unwrap()
        });
        assert_eq!(v, 123);
    }

    #[test]
    fn eval_expr_arithmetic() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        let v = Python::with_gil(|py| {
            eng.eval_expr("2 + 2").unwrap().extract::<i64>(py).unwrap()
        });
        assert_eq!(v, 4);
    }

    #[test]
    fn exec_runtime_error_propagates() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        assert!(eng.exec("raise ValueError('boom')").is_err());
    }

    // ── tf.getvar ─────────────────────────────────────────────────────────

    #[test]
    fn getvar_missing_returns_none() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        let is_none = Python::with_gil(|py| {
            eng.eval_expr("tf.getvar('_py_no_such_var')").unwrap().is_none(py)
        });
        assert!(is_none);
    }

    #[test]
    fn getvar_missing_with_default() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        let v = Python::with_gil(|py| {
            eng.eval_expr("tf.getvar('_py_no_such_var2', 'fallback')")
                .unwrap()
                .extract::<String>(py)
                .unwrap()
        });
        assert_eq!(v, "fallback");
    }

    #[test]
    fn getvar_reads_varstore() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let vars = Arc::new(Mutex::new(VarStore::new()));
        vars.lock().unwrap().set("_py_test_hp", "42");
        let world = Arc::new(Mutex::new(None::<String>));
        let (eng, _rx) = make_engine_with(Arc::clone(&vars), world);
        let v = Python::with_gil(|py| {
            eng.eval_expr("tf.getvar('_py_test_hp')")
                .unwrap()
                .extract::<String>(py)
                .unwrap()
        });
        assert_eq!(v, "42");
    }

    // ── tf.eval ───────────────────────────────────────────────────────────

    #[test]
    fn tf_eval_sends_command() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, rx) = make_engine();
        eng.exec("tf.eval('/echo hello')").unwrap();
        match rx.try_recv().unwrap() {
            PythonCommand::Eval { script } => assert_eq!(script, "/echo hello"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── tf.send ───────────────────────────────────────────────────────────

    #[test]
    fn tf_send_without_world() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, rx) = make_engine();
        eng.exec("tf.send('look')").unwrap();
        match rx.try_recv().unwrap() {
            PythonCommand::Send { text, world } => {
                assert_eq!(text, "look");
                assert!(world.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tf_send_with_world() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, rx) = make_engine();
        eng.exec("tf.send('north', 'mud1')").unwrap();
        match rx.try_recv().unwrap() {
            PythonCommand::Send { text, world } => {
                assert_eq!(text, "north");
                assert_eq!(world.as_deref(), Some("mud1"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── tf.out / tf.err ───────────────────────────────────────────────────

    #[test]
    fn tf_out_sends_command() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, rx) = make_engine();
        eng.exec("tf.out('hello world')").unwrap();
        match rx.try_recv().unwrap() {
            PythonCommand::Out { text } => assert_eq!(text, "hello world"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tf_err_sends_command() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, rx) = make_engine();
        eng.exec("tf.err('oops')").unwrap();
        match rx.try_recv().unwrap() {
            PythonCommand::Err { text } => assert_eq!(text, "oops"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── tf.world ──────────────────────────────────────────────────────────

    #[test]
    fn tf_world_returns_active_world() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let world = Arc::new(Mutex::new(Some("mymud".to_string())));
        let (eng, _rx) = make_engine_with(vars, world);
        let v = Python::with_gil(|py| {
            eng.eval_expr("tf.world()").unwrap().extract::<String>(py).unwrap()
        });
        assert_eq!(v, "mymud");
    }

    #[test]
    fn tf_world_empty_when_none() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        let v = Python::with_gil(|py| {
            eng.eval_expr("tf.world()").unwrap().extract::<String>(py).unwrap()
        });
        assert_eq!(v, "");
    }

    // ── call_func ─────────────────────────────────────────────────────────

    #[test]
    fn call_func_passes_arg_and_returns() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        eng.exec("def _py_test_echo(s): return 'got: ' + s").unwrap();
        let v = Python::with_gil(|py| {
            eng.call_func("_py_test_echo", "hi")
                .unwrap()
                .extract::<String>(py)
                .unwrap()
        });
        assert_eq!(v, "got: hi");
    }

    #[test]
    fn call_func_missing_raises_error() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        assert!(eng.call_func("_py_no_such_fn_xyz", "arg").is_err());
    }

    // ── load_module ───────────────────────────────────────────────────────

    #[test]
    fn load_module_imports_stdlib() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (eng, _rx) = make_engine();
        eng.load_module("os").unwrap();
        let v = Python::with_gil(|py| {
            eng.eval_expr("isinstance(os.sep, str)")
                .unwrap()
                .extract::<bool>(py)
                .unwrap()
        });
        assert!(v);
    }

    // ── run_file ──────────────────────────────────────────────────────────

    #[test]
    fn run_file_executes_script() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "_py_test_from_file = 'yes'").unwrap();
        let (eng, _rx) = make_engine();
        eng.run_file(f.path()).unwrap();
        let v = Python::with_gil(|py| {
            eng.eval_expr("_py_test_from_file")
                .unwrap()
                .extract::<String>(py)
                .unwrap()
        });
        assert_eq!(v, "yes");
    }

    // ── drop clears state ─────────────────────────────────────────────────

    #[test]
    fn drop_disables_tf_callbacks() {
        let _g = TEST_MX.lock().unwrap_or_else(|p| p.into_inner());
        let (tx_old, rx_old) = std::sync::mpsc::sync_channel(64);
        let vars = Arc::new(Mutex::new(VarStore::new()));
        let world = Arc::new(Mutex::new(None::<String>));
        let eng = PythonEngine::new(Arc::clone(&vars), tx_old, world).unwrap();
        drop(eng);

        // Re-attach with a new channel; Python stays alive.
        let (eng2, rx_new) = make_engine();
        eng2.exec("tf.eval('test')").unwrap();

        // Old channel received nothing (engine was dropped before send).
        assert!(rx_old.try_recv().is_err());
        // New channel got the command.
        match rx_new.try_recv().unwrap() {
            PythonCommand::Eval { script } => assert_eq!(script, "test"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
