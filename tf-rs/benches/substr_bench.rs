use criterion::{black_box, criterion_group, criterion_main, Criterion};
use aho_corasick::AhoCorasickBuilder;

fn old_alloc_unicode(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

fn build_ac(pattern: &str) -> aho_corasick::AhoCorasick {
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .build([pattern])
}

fn ac_is_match(ac: &aho_corasick::AhoCorasick, hay: &str) -> bool {
    ac.is_match(hay)
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
    let ac = build_ac(needle);

    let mut g = c.benchmark_group("substr_compare");

    g.bench_function("old_alloc_unicode_small", |b| {
        b.iter(|| old_alloc_unicode(black_box(&hay_small), black_box(needle)))
    });
    g.bench_function("ac_small", |b| {
        b.iter(|| ac_is_match(black_box(&ac), black_box(&hay_small)))
    });

    g.bench_function("old_alloc_unicode_med", |b| {
        b.iter(|| old_alloc_unicode(black_box(&hay_med), black_box(needle)))
    });
    g.bench_function("ac_med", |b| {
        b.iter(|| ac_is_match(black_box(&ac), black_box(&hay_med)))
    });

    g.bench_function("old_alloc_unicode_large", |b| {
        b.iter(|| old_alloc_unicode(black_box(&hay_large), black_box(needle)))
    });
    g.bench_function("ac_large", |b| {
        b.iter(|| ac_is_match(black_box(&ac), black_box(&hay_large)))
    });

    g.finish();
}

criterion_group!(benches, bench_substr);
criterion_main!(benches);
