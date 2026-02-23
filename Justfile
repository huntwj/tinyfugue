TFLIBDIR := justfile_directory() / "lib/tf"

# Run the TinyFugue Rust binary (Phase 15 cutover).
run:
    TFLIBDIR="{{ TFLIBDIR }}" cargo run --bin tf

# Build the Rust binary.
build:
    cargo build

# Run the full test suite.
test:
    cargo test

# Run clippy (must be clean before every commit).
clippy:
    cargo clippy

# ── Legacy C targets (archived) ──────────────────────────────────────────────
# The original C binary lives in src/ and requires autoconf + system libraries.
# See CLAUDE.md for configure options.

build-c:
    make

run-c:
    TFLIBDIR="{{ TFLIBDIR }}" src/tf
