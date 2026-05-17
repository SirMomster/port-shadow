# port-shadow justfile
# Install just: https://github.com/casey/just

# Default: show available recipes
default:
    @just --list

# ── Local build ──────────────────────────────────────────────────────────────

# Build debug binary for the current host
build:
    cargo build

# Build optimized release binary for the current host
release:
    cargo build --release

# Run tests
test:
    cargo test

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Format source code
fmt:
    cargo fmt

# Check formatting without modifying files
fmt-check:
    cargo fmt --check

# ── Cross-compilation ─────────────────────────────────────────────────────────
# Requires cross: cargo install cross
# Or install the required cross-compilers manually (see README).

# Build for all supported targets
build-all: build-linux-x86 build-linux-aarch64 build-mac-aarch64 build-windows-x86

# Linux x86_64
build-linux-x86:
    cross build --release --target x86_64-unknown-linux-gnu

# Linux aarch64 (e.g. AWS Graviton, Raspberry Pi 4)
build-linux-aarch64:
    cross build --release --target aarch64-unknown-linux-gnu

# macOS Apple Silicon (M1/M2/M3)
# Note: cross does not support macOS targets; use native build on a Mac
# or GitHub Actions with macos-latest runner.
build-mac-aarch64:
    cargo build --release --target aarch64-apple-darwin

# Windows x86_64 (requires mingw toolchain)
build-windows-x86:
    cross build --release --target x86_64-pc-windows-gnu

# ── Artifacts ────────────────────────────────────────────────────────────────

# Collect all release binaries into ./dist/
dist: build-all
    mkdir -p dist
    cp target/x86_64-unknown-linux-gnu/release/port-shadow   dist/port-shadow-linux-x86_64
    cp target/aarch64-unknown-linux-gnu/release/port-shadow  dist/port-shadow-linux-aarch64
    cp target/x86_64-pc-windows-gnu/release/port-shadow.exe  dist/port-shadow-windows-x86_64.exe
    @echo "Binaries written to ./dist/"

# ── Development helpers ───────────────────────────────────────────────────────

# Run with example config
run *ARGS:
    cargo run -- {{ARGS}}

# Install binary to ~/.cargo/bin
install:
    cargo install --path .

# Show current toolchain targets
targets:
    rustup target list --installed
