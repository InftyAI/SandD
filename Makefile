.PHONY: help build install dev test clean daemon-build daemon-release

help:
	@echo "SandD - Sandbox Daemon - Build Commands"
	@echo ""
	@echo "  make build          - Build Python package (debug mode)"
	@echo "  make install        - Install Python package locally"
	@echo "  make dev            - Install in development mode with hot reload"
	@echo "  make test           - Run tests"
	@echo "  make daemon-build   - Build daemon binary (debug)"
	@echo "  make daemon-release - Build daemon binary (release)"
	@echo "  make clean          - Clean build artifacts"

build:
	maturin build -m server/Cargo.toml

release:
	maturin develop --release -m server/Cargo.toml

dev:
	maturin develop -m server/Cargo.toml

test:
	pytest tests/

daemon-build:
	cargo build --package sandd

daemon-release:
	cargo build --package sandd --release
	@echo ""
	@echo "SandD binary built at: ./target/release/sandd"

clean:
	cargo clean
	rm -rf target/
	rm -rf python/sandd.egg-info/
	find . -type d -name __pycache__ -exec rm -rf {} + 2>/dev/null || true
