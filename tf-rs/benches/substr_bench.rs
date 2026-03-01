use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tf::pattern::{MatchMode, Pattern};

fn old_alloc_unicode(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

fn new_pattern_match(pat: &Pattern, hay: &str) -> bool {
    pat.matches(hay)
}

fn make_hay(repeats: usize) -> String {
    let chunk = "The quick brown FOX jumps over the lazy dog. ";
    chunk.repeat(repeats)
}

fn bench_substr(c: &mut Criterion) {
    let hay_small = make_hay(100); // ~4.5k
    let hay_med = make_hay(1000); // ~45k
    let hay_large = make_hay(10000); // ~450k

    let needle = "lazy";
    let pat = Pattern::new(needle, MatchMode::Substr).unwrap();

    let mut g = c.benchmark_group("substr_compare");

    g.bench_function("old_alloc_unicode_small", |b| {
        b.iter(|| old_alloc_unicode(black_box(&hay_small), black_box(needle)))
    });
    g.bench_function("new_ascii_ci_small", |b| {
        b.iter(|| new_pattern_match(black_box(&pat), black_box(&hay_small)))
    });

    g.bench_function("old_alloc_unicode_med", |b| {
        b.iter(|| old_alloc_unicode(black_box(&hay_med), black_box(needle)))
    });
    g.bench_function("new_ascii_ci_med", |b| {
        b.iter(|| new_pattern_match(black_box(&pat), black_box(&hay_med)))
    });

    g.bench_function("old_alloc_unicode_large", |b| {
        b.iter(|| old_alloc_unicode(black_box(&hay_large), black_box(needle)))
    });
    g.bench_function("new_ascii_ci_large", |b| {
        b.iter(|| new_pattern_match(black_box(&pat), black_box(&hay_large)))
    });

    g.finish();
}

criterion_group!(benches, bench_substr);
criterion_main!(benches);
