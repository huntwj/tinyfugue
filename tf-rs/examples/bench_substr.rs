use std::time::Instant;

/// Old, allocation-heavy method: Unicode `to_lowercase()` on both hay and needle.
fn old_alloc_unicode(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// New, allocation-free ASCII case-insensitive search where `needle_lo` is
/// already ASCII-lowercased.
fn new_noalloc_ascii(hay: &str, needle_lo: &str) -> bool {
    let h = hay.as_bytes();
    let n = needle_lo.as_bytes();
    if n.is_empty() {
        return true;
    }
    if n.len() > h.len() {
        return false;
    }
    let first = n[0];
    for i in 0..=h.len() - n.len() {
        if h[i].to_ascii_lowercase() != first {
            continue;
        }
        let mut ok = true;
        for j in 1..n.len() {
            if h[i + j].to_ascii_lowercase() != n[j] {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
    }
    false
}

fn make_hay(repeats: usize) -> String {
    let chunk = "The quick brown FOX jumps over the lazy dog. ";
    chunk.repeat(repeats)
}

fn bench<F>(name: &str, f: F, hay: &str, needle: &str, iters: usize)
where
    F: Fn(&str, &str) -> bool,
{
    // Warm-up (if needle may not be present, don't assert)
    let _ = f(hay, needle);

    let start = Instant::now();
    for _ in 0..iters {
        let _ = f(hay, needle);
    }
    let dur = start.elapsed();
    let micros = dur.as_secs_f64() * 1_000_000.0;
    println!(
        "{:<28} {:>10} iters — {:>9.0} µs total, {:>6.2} µs/iter",
        name,
        iters,
        micros,
        micros / (iters as f64)
    );
}

fn main() {
    let sizes = [100usize, 1_000, 10_000];
    let patterns = [
        ("start", "The"),
        ("middle", "FOX"),
        ("end", "dog"),
        ("missing", "NOPE"),
    ];

    for &sz in &sizes {
        let hay = make_hay(sz);
        println!("\nHay size: {} bytes (repeats={})", hay.len(), sz);
        for (label, needle) in &patterns {
            let iters = match sz {
                100 => 2000,
                1_000 => 500,
                _ => 200,
            };
            let needle_lo = needle.to_ascii_lowercase();
            println!("Pattern: {} ('{}') — iters={}", label, needle, iters);
            bench("old_alloc_unicode", old_alloc_unicode, &hay, needle, iters);
            bench(
                "new_noalloc_ascii",
                |h, _| new_noalloc_ascii(h, &needle_lo),
                &hay,
                needle,
                iters,
            );
        }
    }
}
