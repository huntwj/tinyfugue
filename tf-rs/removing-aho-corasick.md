# Removing AhoCorasick from the Substr Pattern Matcher

## Background

The `Pattern` type in `tf-rs/src/pattern.rs` supports four match modes.
The `MatchMode::Substr` mode performs case-insensitive substring search —
the same as the C TF `MATCH_SUBSTR` flag.

The original Rust implementation compiled each `Substr` pattern into an
`AhoCorasick` automaton from the `aho-corasick` crate.

## Why AhoCorasick Was Used

The `aho_corasick` crate provides efficient multi-pattern substring search.
When you need to match a haystack against hundreds of patterns simultaneously
(e.g., a spam filter with 500 blocked words), AhoCorasick shines: it scans
the haystack once in O(n) time regardless of pattern count, where n is the
length of the input text.

In the original implementation, the intent was presumably to leverage the
crate's built-in `ascii_case_insensitive` option rather than rolling a custom
case-insensitive scan.

## Why We Removed It

TinyFugue's `Substr` mode is always **single-pattern**: each `Pattern` object
holds exactly one substring to match.  The AhoCorasick algorithm provides its
real benefit only when matching _many_ patterns simultaneously.

For a single pattern:

1. **Build cost is wasted.**  The AhoCorasick automaton state machine must be
   constructed for every compiled `Pattern` even though the structure will
   never be reused across different haystacks or different patterns.  For a
   MUD client where triggers are compiled once but fired thousands of times,
   the build cost is amortized — but the object still occupies heap memory
   and requires a heap allocation per pattern.

2. **The benefit is absent.**  AhoCorasick scans the haystack in one linear
   pass looking for any of its N patterns.  With N=1 that degenerates to a
   simple substring search — no better than a hand-written O(n·m) scan using
   CPU-cache-friendly byte comparisons.

3. **Arc overhead.**  After the `L11` fix (Pattern Clone via Arc), each
   `Compiled::Substr` variant held an `Arc<AhoCorasick>`, adding two pointer
   indirections and atomic reference-count overhead on every match.

## What Replaced It

A private helper `substr_find_ascii_ci(text, lo_pattern)` performs a
straightforward O(n·m) scan:

```rust
'outer: for i in 0..=tb.len().saturating_sub(pb.len()) {
    for (j, &p) in pb.iter().enumerate() {
        if tb[i + j].to_ascii_lowercase() != p {
            continue 'outer;
        }
    }
    return Some((i, i + pb.len()));
}
```

The pattern is stored pre-lowercased in `Compiled::Substr(String)` at
compile time so `to_ascii_lowercase()` on the pattern bytes never runs
during matching — only the haystack bytes are lowercased on the fly.

This:
- Eliminates the heap allocation for the AC state machine.
- Eliminates the `Arc` indirection.
- Removes the `aho-corasick = "0.7"` dependency from `Cargo.toml`.
- Keeps correctness identical: case-insensitive ASCII substring search.

## Trade-offs

The new scan is O(n·m) in the worst case.  For a pathological haystack
(`"aaaaaaaaab"` with pattern `"ab"`) this is slower than AhoCorasick.
In practice, MUD output lines are short (< 500 bytes) and trigger patterns
are also short (< 50 chars), so the worst-case quadratic term never
materialises.

If TinyFugue ever needs to match a single haystack against _many_ `Substr`
patterns simultaneously, re-introducing AhoCorasick (or `memchr::memmem`)
at the `MacroStore` level (not the `Pattern` level) would be the right
architectural move.
