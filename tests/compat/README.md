# Reticulum-rs Python Compatibility Tests

This directory contains tests to verify compatibility between the Rust and Python implementations of Reticulum.

## Setup

```bash
# Install Python dependencies using uv
cd tests/compat
uv venv
source .venv/bin/activate  # or .venv\Scripts\activate on Windows
uv pip install -e ".[dev]"
```

## Running Tests

### Basic Compatibility Tests
```bash
# Run Python-only format tests
pytest test_formats.py -v

# Run cross-implementation tests (requires Rust binary)
pytest test_interop.py -v

# Run all tests
pytest -v
```

### Individual Test Categories

```bash
# Test packet formats
pytest test_formats.py::test_identity_format -v

# Test cryptography
pytest test_formats.py::test_encryption_decryption -v

# Test announces
pytest test_formats.py::test_announce_format -v
```

## Test Structure

- `test_formats.py` - Tests for binary format compatibility (packets, identities, etc.)
- `test_crypto.py` - Tests for cryptographic compatibility
- `test_interop.py` - Tests for runtime interoperability (Rust ↔ Python communication)
- `helpers.py` - Shared test utilities
- `rust_binary.py` - Helper to run Rust test binaries

## Test Data

Test vectors and expected values are generated from the Python reference implementation to ensure Rust matches exactly.
