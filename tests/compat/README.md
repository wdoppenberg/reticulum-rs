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

### Quick Start - Interoperability Tests
```bash
# Run interoperability tests with automatic setup
./run_interop_tests.sh
```

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
- `test_interop.py` - **Tests for runtime interoperability (Rust ↔ Python communication)**
  - Tests Rust server with Python client
  - Tests Python server with Rust client
  - Validates bidirectional network connectivity
- `conftest.py` - Pytest fixtures and shared setup
- `run_interop_tests.sh` - Convenience script to run interop tests

## Test Data

Test vectors and expected values are generated from the Python reference implementation to ensure Rust matches exactly.

## Interoperability Test Details

The `test_interop.py` file contains two main test scenarios:

1. **Rust Server + Python Client**: Starts a Rust Reticulum instance as a TCP server, then connects a Python RNS client to verify connectivity.

2. **Python Server + Rust Client**: Starts a Python RNS instance as a TCP server, then connects a Rust Reticulum client to verify connectivity.

These tests verify that both implementations can successfully establish network connections and communicate at the transport layer.
