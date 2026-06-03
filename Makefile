RUFF := .venv/bin/ruff
PYTEST := .venv/bin/pytest
MATURIN := .venv/bin/maturin

.PHONY: help build install dev test clean daemon-build daemon-release test-e2e docker-build docker-down

help:
	@echo "SandD - Sandbox Daemon - Build Commands"
	@echo ""
	@echo "  make build          - Build Python package (debug mode)"
	@echo "  make install        - Install Python package locally"
	@echo "  make dev            - Install in development mode with hot reload"
	@echo "  make test           - Run unit and integration tests"
	@echo "  make test-e2e       - Run end-to-end tests with Docker"
	@echo "  make daemon-build   - Build daemon binary (debug)"
	@echo "  make daemon-release - Build daemon binary (release)"
	@echo "  make docker-build   - Build Docker image for daemon"
	@echo "  make docker-down    - Stop and remove Docker containers"
	@echo "  make clean          - Clean build artifacts"

build: $(MATURIN)
	$(MATURIN) build -m server/Cargo.toml

release: $(MATURIN)
	$(MATURIN) develop --release -m server/Cargo.toml

dev: $(MATURIN)
	$(MATURIN) develop -m server/Cargo.toml

test: lint $(PYTEST) dev
	@echo "Running Rust tests (daemon)..."
	cargo test --package sandd
	@echo ""
	@echo "Running Rust tests (server protocol)..."
	cargo test --package sandbox-server --lib
	@echo ""
	@echo "Running Python tests..."
	$(PYTEST) python/tests/

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

test-e2e: $(PYTEST) dev
	@echo "Building Docker images..."
	docker compose -f docker-compose.e2e.yml build
	@echo ""
	@echo "Running E2E tests with Docker..."
	$(PYTEST) python/tests/test_e2e.py -v -s
	@echo ""
	@echo "Cleaning up containers..."
	docker compose -f docker-compose.e2e.yml down

docker-build:
	docker compose -f docker-compose.e2e.yml build

docker-up:
	docker compose -f docker-compose.e2e.yml up -d

docker-down:
	docker compose -f docker-compose.e2e.yml down

.PHONY: lint
lint: $(RUFF)
	$(RUFF) check .

$(RUFF):
	@echo "Installing ruff..."
	@python3 -m venv .venv || true
	@.venv/bin/pip install --quiet ruff
	@echo "Ruff installed successfully"

$(PYTEST):
	@echo "Installing pytest..."
	@python3 -m venv .venv || true
	@.venv/bin/pip install --quiet pytest pytest-asyncio
	@echo "Pytest installed successfully"

$(MATURIN):
	@echo "Installing maturin..."
	@python3 -m venv .venv || true
	@.venv/bin/pip install --quiet maturin
	@echo "Maturin installed successfully"
