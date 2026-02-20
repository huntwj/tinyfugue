TFLIBDIR := justfile_directory() / "lib/tf"

run:
    TFLIBDIR="{{ TFLIBDIR }}" src/tf
