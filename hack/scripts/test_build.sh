#!/bin/bash
set -e

echo "==================================="
echo "SandD - Sandbox Daemon - Build Test"
echo "==================================="
echo ""

echo "Step 1: Check Rust installation..."
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust not found. Install from: https://rustup.rs/"
    exit 1
fi
echo "✓ Rust found: $(rustc --version)"
echo ""

echo "Step 2: Check Python installation..."
if ! command -v python3 &> /dev/null; then
    echo "❌ Python3 not found."
    exit 1
fi
echo "✓ Python found: $(python3 --version)"
echo ""

echo "Step 3: Install maturin..."
pip3 install maturin >/dev/null 2>&1 || true
if ! command -v maturin &> /dev/null; then
    echo "❌ Maturin installation failed"
    exit 1
fi
echo "✓ Maturin installed"
echo ""

echo "Step 4: Build Python package..."
maturin develop --release -m server/Cargo.toml
echo "✓ Python package built"
echo ""

echo "Step 5: Build daemon binary (release)..."
cargo build --package sandd --release
echo "✓ SandD binary built at: target/release/sandd"
echo ""

echo "Step 6: Test Python import..."
if [ -f ".venv/bin/python" ]; then
    PYTHON=".venv/bin/python"
else
    PYTHON="python3"
fi
$PYTHON -c "from sandd import Server; print('✓ Import successful')"
echo ""

echo "==================================="
echo "✅ All builds successful!"
echo "==================================="
echo ""
echo "Next steps:"
echo "  1. Start agent:  python3 examples/agent_example.py"
echo "  2. Start daemon: ./target/release/sandd --server-url ws://localhost:8765/ws"
echo ""
