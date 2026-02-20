# Rust Code Review — tinyfugue/tf-rs

# Rust Code Review — tinyfugue/tf-rs

Date: 2026-02-20
Scope: `tf-rs` crate (library + script engine + pattern & string utilities)

## Summary (high level)

- Memory safety: No uses of `unsafe`, raw pointers, `transmute`, or FFI were found. The codebase appears memory-safe by Rust rules.
- Correctness: Overall solid. A few APIs may panic on out-of-range indexing or rely on assumptions that should be hardened.
- Idiomatic Rust: Mostly idiomatic. A few places use intermediate `Vec<char>` where alternatives could avoid allocations; some small ergonomic/clarity improvements are recommended.
- Tests: Good unit test coverage across modules (script, pattern, tfstr, builtins). Additional edge-case and integration tests are suggested.
- Documentation: Public APIs are documented; a few panic conditions would benefit from clearer docs or returning `Option/Result`.

---

## Files inspected (representative)

- `tf-rs/src/tfstr.rs` — owned string type with per-char attributes
- `tf-rs/src/pattern.rs` — pattern compilation + matching (regex, glob, substr)
- `tf-rs/src/script/*` — expression parser, interpreter, builtins, value type, expand
- `tf-rs/src/world.rs`, `config.rs`, `var.rs` — config/world helpers

I scanned the crate for `unsafe`, `transmute`, `from_raw`, `extern`, raw `ptr::` usage and found none. I searched for `unwrap`/`expect` to find places that may panic at runtime.

---

## Memory safety (HIGH priority)

Findings:

- No `unsafe` blocks, raw pointers, `transmute`, or FFI calls were detected in `tf-rs` — good.
- Allocations and ownership follow Rust conventions (owned `String`, `Vec<Attr>`, boxed Aho-Corasick, etc.).

Points to watch / recommendations:

- `TfStr::attr_at(&self, n: usize) -> Option<Attr>` currently does `v[n]` and documents that it will panic if `n >= self.char_count()`. Consider:
  - Returning `Option<Attr>` in a way that does not panic for out-of-range requests (i.e. check bounds first) or rename to make the panic expectation explicit (e.g., `attr_at_unchecked`). Safer API usage reduces accidental panics.
- Indexing into buffers with `v[i]` appears in a few places where the code logic ensures safety (e.g., `Pattern::find` uses `caps.get(0).unwrap()` only when `captures()` returned Some). Documenting these invariants helps reviewers and future maintainers.

Overall: memory-safety is strong; no urgent unsafe-related issues.

---

## Correctness (HIGH priority)

Findings & suggestions:

- `Pattern::matches` does ASCII-case-insensitive comparison for `MatchMode::Simple` by using `as_bytes()` and `eq_ignore_ascii_case`. That is correct for ASCII but will not do Unicode case-folding; this matches the C implementation likely, but note the limitation in docs if Unicode case-insensitivity is ever expected.
- Many parser/tokenizer locations use `.unwrap()` when advancing characters (e.g. in `expr.rs` & `expand.rs`). These appear to be in paths where a valid input invariant is assumed; consider converting parsing code to return `Result` or explicitly assert with an informative error message when invariants are violated.
- `builtins::pad` uses `args.get(2).map(|v| v.as_str().chars().next().unwrap_or(' ')).unwrap_or(' ')`. This is safe, but slightly dense; consider a small helper function `arg_char_or_default(args, idx, default)`.
- `call_builtin` returns `Option<Result<Value,String>>` and tests call `.expect("not a builtin").expect("call failed")`. This double-`expect` is confined to test helpers, but in production paths where builtins are invoked consider returning `Result` or propagating errors instead of unwrapping.

### Edge cases to test / fix

- Parser resilience: add unit tests for malformed inputs to ensure the parser returns informative errors rather than panics. Example inputs: unterminated quotes, unmatched parentheses, invalid escape sequences.
- Unicode behavior: add tests that exercise non-ASCII input for code paths that currently assume ASCII (e.g., `MatchMode::Simple` ASCII-only case handling, `pad` behavior when pad char is multi-byte).
- Boundary values: for `substr`, `pad`, and index conversions, add tests for negative, zero, and very large indices.

---

## Idiomatic Rust (MEDIUM priority)

Suggestions:

- Avoid unnecessary intermediate allocations when possible. Examples: some `substr` implementations collect into `Vec<char>` to slice by character index. This is correct but allocates; consider iterator-based slicing or alternative strategies when performance is critical.
- Small helpers improve clarity: repeated argument-access patterns (`get_str/get_int/get_float`) are good; consider a tiny helper for `arg_char_or_default` to simplify `pad` and similar code.
- Prefer explicit `Result` propagation in parser/driver code instead of `expect`/`unwrap` in non-test code paths.

---

## Tests and coverage (MEDIUM priority)

Findings:

- Unit tests are numerous and exercise many functions (builtins, pattern matching, TfStr, script evaluation). Good baseline coverage.
- Missing: integration tests that combine parsing + execution + builtins on realistic scripts; fuzzing or property tests for the parser would catch edge-case panics.

Recommendations:

- Add tests for malformed inputs and boundary cases (see "Edge cases" above).
- Consider adding property-based tests (e.g., with `proptest`) for parser/tokenizer to assert round-trip or error properties.

---

## Self-documenting code & docs (LOW priority)

- Public APIs are generally documented. For panic-prone methods (e.g. `TfStr::attr_at`) make the panic behavior explicit in the docstring or return `Option` to avoid panics.

---

## Actionable next steps (prioritized)

1. Change `TfStr::attr_at` to avoid panicking on out-of-range access or document the panic and rename to indicate unchecked behavior. (Memory safety / correctness)
2. Convert parser/tokenizer error paths to return `Result` with useful messages and add unit tests for malformed inputs. (Correctness)
3. Add integration tests combining parsing, expansion, and builtin calls to exercise the interpreter end-to-end. (Tests)
4. Consider replacing `Vec<char>` allocations in hot paths with iterator-based approaches or document the performance tradeoffs. (Idiomatic / performance)

---

## Final notes

This crate is well-structured and follows Rust ownership/borrowing rules; I found no unsafe or FFI concerns. The main opportunities are hardening APIs against panics, expanding tests for edge cases, and a few minor idiomatic improvements.

If you want, I can implement item 1 (`TfStr::attr_at` change) and add a small suite of parser edge-case tests as a follow-up.
