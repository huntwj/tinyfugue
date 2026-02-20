TFLIBDIR := justfile_directory() / "lib/tf"

run:
    TFLIBDIR="{{ TFLIBDIR }}" src/tf

build-rust:
    cargo build

run-rust:
    cargo run --bin tf-rust
