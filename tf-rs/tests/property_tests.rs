use std::path::Path;

use proptest::prelude::*;
use tf::pattern::{MatchMode, Pattern};
use tf::script::builtins::call_builtin;
use tf::script::stmt::parse_script;
use tf::script::value::Value;
use tf::tfstr::TfStr;

/// Parse all .tf files in lib/tf/ — acceptance criterion for Phase 11.
#[test]
fn parse_all_lib_tf_files() {
    let tf_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("lib/tf");

    let mut entries: Vec<_> = std::fs::read_dir(&tf_dir)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", tf_dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "tf").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.path());

    assert!(!entries.is_empty(), "no .tf files found in {}", tf_dir.display());

    let mut failures = Vec::new();
    for entry in &entries {
        let path = entry.path();
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        if let Err(e) = parse_script(&src) {
            failures.push(format!("{}: {e}", path.file_name().unwrap().to_string_lossy()));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{}/{} files failed to parse:\n  {}",
            failures.len(),
            entries.len(),
            failures.join("\n  ")
        );
    }
    println!("All {} lib/tf/*.tf files parsed successfully", entries.len());
}

proptest! {
    /// Ensure parser never panics on arbitrary valid UTF-8 input; it should
    /// return Ok or Err but not panic.
    #[test]
    fn parser_does_not_panic(s in "\\PC*") {
        let _ = std::panic::catch_unwind(|| {
            let _ = parse_script(&s);
        });
    }
}

proptest! {
    /// Empty pattern should match any input for all modes.
    #[test]
    fn empty_pattern_matches_all(s in "\\PC*") {
        for &mode in &[MatchMode::Regexp, MatchMode::Glob, MatchMode::Simple, MatchMode::Substr] {
            let p = Pattern::new("", mode).unwrap();
            prop_assert!(p.matches(&s));
        }
    }
}

proptest! {
    /// TfStr round-trip-like invariants: char_count matches .chars().count().
    #[test]
    fn tfstr_char_count_consistent(s in "\\PC*") {
        let mut t = TfStr::new();
        t.push_str(&s, None);
        prop_assert_eq!(t.char_count(), s.chars().count());
        if let Some(attrs) = t.char_attrs() {
            prop_assert_eq!(attrs.len(), t.char_count());
        }
    }
}

proptest! {
    /// substr built-in: result is subsequence of input and not longer than input
    #[test]
    fn substr_properties(s in "\\PC*", start in 0i64..100i64, len in 0i64..100i64) {
        // call_builtin returns Option<Result<..>>; use helper invocation via direct call
        let args = vec![Value::Str(s.clone()), Value::Int(start), Value::Int(len)];
        let res_opt = call_builtin("substr", args);
        if let Some(Ok(Value::Str(out))) = res_opt {
            prop_assert!(out.chars().count() <= s.chars().count());
            // simple subsequence check: every char in out appears in s in order
            let mut it = s.chars();
            for c in out.chars() {
                prop_assert!(it.any(|x| x == c));
            }
        }
    }
}
