//! Global variable store.
//!
//! Corresponds to the global `var_table` in `variable.c`.
//! Special/typed variables and the local variable stack are Phase 4 concerns;
//! this module models the plain string-valued global table used during config
//! loading.

use std::collections::HashMap;

/// Global key/value variable store.
#[derive(Debug, Default)]
pub struct VarStore {
    vars: HashMap<String, String>,
}

impl VarStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or overwrite) a variable.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(name.into(), value.into());
    }

    /// Get the string value of a variable.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// Get the value of a variable parsed as an integer.
    pub fn get_int(&self, name: &str) -> Option<i64> {
        self.vars.get(name)?.parse().ok()
    }

    /// Remove a variable.  Returns `true` if it existed.
    pub fn unset(&mut self, name: &str) -> bool {
        self.vars.remove(name).is_some()
    }

    /// Returns `true` if the variable is set.
    pub fn contains(&self, name: &str) -> bool {
        self.vars.contains_key(name)
    }

    /// Iterate over all variables.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter()
    }

    pub fn len(&self) -> usize {
        self.vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut vars = VarStore::new();
        vars.set("wrap", "1");
        assert_eq!(vars.get("wrap"), Some("1"));
    }

    #[test]
    fn overwrite() {
        let mut vars = VarStore::new();
        vars.set("x", "old");
        vars.set("x", "new");
        assert_eq!(vars.get("x"), Some("new"));
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn get_int() {
        let mut vars = VarStore::new();
        vars.set("tabsize", "8");
        assert_eq!(vars.get_int("tabsize"), Some(8));
    }

    #[test]
    fn get_int_non_numeric_returns_none() {
        let mut vars = VarStore::new();
        vars.set("name", "hello");
        assert_eq!(vars.get_int("name"), None);
    }

    #[test]
    fn unset() {
        let mut vars = VarStore::new();
        vars.set("gone", "bye");
        assert!(vars.unset("gone"));
        assert_eq!(vars.get("gone"), None);
        assert!(!vars.unset("gone")); // already gone
    }

    #[test]
    fn missing_returns_none() {
        let vars = VarStore::new();
        assert_eq!(vars.get("nope"), None);
        assert!(!vars.contains("nope"));
    }

    #[test]
    fn contains() {
        let mut vars = VarStore::new();
        vars.set("present", "yes");
        assert!(vars.contains("present"));
        assert!(!vars.contains("absent"));
    }
}
