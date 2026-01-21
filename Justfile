# Justfile for reticulum-rs
# Run `just --list` to see all available commands

# Default recipe - show available commands
default:
    @just --list

# Run all tests
test:
    cargo test --workspace --all-features

# Run tests with output
test-verbose:
    cargo test --workspace --all-features -- --nocapture

# Run tests for a specific package
test-package package:
    cargo test -p {{package}} --all-features

# Run a specific test
test-one test-name:
    cargo test {{test-name}} --workspace --all-features -- --nocapture

# Run clippy lints
lint:
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# Run clippy with automatic fixes
lint-fix:
    cargo clippy --workspace --all-features --all-targets --fix --allow-dirty

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Format code
fmt:
    cargo fmt --all

# Run all quality checks (fmt, lint, test)
check: fmt-check lint test

# Build all workspace members
build:
    cargo build --workspace --all-features

# Build in release mode
build-release:
    cargo build --workspace --all-features --release

# Run examples
example name:
    cargo run --example {{name}}

# Run the basic example
run-basic:
    cargo run --example reticulum_basic

# Clean build artifacts
clean:
    cargo clean

# Update dependencies
update:
    cargo update

# Check for outdated dependencies
outdated:
    cargo outdated

# Generate documentation
doc:
    cargo doc --workspace --all-features --no-deps --open

# Run Python compatibility tests
test-compat:
    #!/usr/bin/env bash
    cd tests/compat && ./run_interop_tests.sh

# Run only Python format tests (no interop)
test-compat-formats:
    #!/usr/bin/env bash
    cd tests/compat && source .venv/bin/activate && pytest test_formats.py -v

# Run only Python crypto tests
test-compat-crypto:
    #!/usr/bin/env bash
    cd tests/compat && source .venv/bin/activate && pytest test_crypto.py -v

# Run only interop tests
test-interop:
    #!/usr/bin/env bash
    cd tests/compat && source .venv/bin/activate && pytest test_interop.py -v -s

# Setup Python test environment
setup-python:
    #!/usr/bin/env bash
    cd tests/compat && \
    if [ ! -d ".venv" ]; then \
        uv venv; \
    fi && \
    source .venv/bin/activate && \
    uv pip install -e ".[dev]"

# Run all tests (Rust + Python)
test-all: test test-compat

# Check dependencies for security vulnerabilities
audit:
    cargo audit

# Benchmark
bench:
    cargo bench --workspace

# Watch for changes and run tests
watch-test:
    cargo watch -x "test --workspace --all-features"

# Watch for changes and run checks
watch-check:
    cargo watch -x "check --workspace --all-features"

# Install development tools
install-dev-tools:
    cargo install cargo-watch cargo-audit cargo-outdated

# Run a release build and strip symbols
build-stripped:
    cargo build --workspace --release && \
    strip target/release/reticulum* 2>/dev/null || true
