#!/bin/bash
# Run interoperability tests between Rust and Python Reticulum implementations

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Setting up Python environment..."

# Check if virtual environment exists
if [ ! -d ".venv" ]; then
    echo "Creating virtual environment..."
    uv venv
fi

# Activate virtual environment
source .venv/bin/activate

# Install dependencies
echo "Installing Python dependencies..."
uv pip install -e ".[dev]"

# Run interoperability tests
echo ""
echo "Running interoperability tests..."
echo "================================"
pytest test_interop.py -v -s

echo ""
echo "Tests completed!"
