//! TF scripting language — Phase 4.
//!
//! This module implements a tree-walking interpreter for TF's built-in
//! scripting language, covering:
//!
//! - Variable substitution (`%{name}`, `{n}`, `$[expr]`, …)
//! - Arithmetic and string expressions
//! - Control flow: `/if` … `/else` … `/endif`, `/while` … `/done`, `/for`
//! - `/let`, `/set`, `/unset`, `/return`, `/break`, `/echo`, `/send`
//! - ~30 built-in functions (string, math, type inspection)
//! - User-defined macros via [`Interpreter::define_macro`]
//!
//! # Quick start
//!
//! ```rust
//! use tf::script::Interpreter;
//!
//! let mut interp = Interpreter::new();
//! interp.exec_script("/set x=6\n/echo $[x * 7]").unwrap();
//! assert_eq!(interp.output, vec!["42"]);
//! ```

pub mod builtins;
pub mod expand;
pub mod expr;
pub mod interp;
pub mod stmt;
pub mod value;

// Re-exports for convenience.
pub use expr::EvalContext;
pub use interp::Interpreter;
pub use value::Value;
