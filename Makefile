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
	@echo "  make test           - Run unit and integration tests (fast, no Docker)"
	@echo "  make test-e2e       - Run end-to-end tests with Docker (slow)"
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
	@echo "Running Python tests (excluding e2e)..."
	$(PYTEST) python/tests/ -m "not e2e"

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
	docker compose -f hack/docker/docker-compose.e2e.yml build
	@echo ""
	@echo "Running E2E tests with Docker..."
	$(PYTEST) python/tests/ -m e2e -v -s
	@echo ""
	@echo "Cleaning up containers..."
	docker compose -f hack/docker/docker-compose.e2e.yml down

docker-build:
	docker compose -f hack/docker/docker-compose.e2e.yml build

docker-up:
	docker compose -f hack/docker/docker-compose.e2e.yml up -d

docker-down:
	docker compose -f hack/docker/docker-compose.e2e.yml down

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

.PHONY: build-wheels build-wheels-local build-wheels-linux publish-pypi

# Build wheel for current platform only
build-wheels-local: $(MATURIN)
	@echo "Building wheel for current platform..."
	$(MATURIN) build --release -m server/Cargo.toml

# Build Linux wheels using Docker
build-wheels-linux:
	@echo "Building Linux wheels using Docker..."
	@command -v docker >/dev/null 2>&1 || { echo "Error: Docker not found"; exit 1; }
	@echo "Building for Linux x86_64..."
	docker run --rm --platform linux/amd64 -v $$(pwd):/io ghcr.io/pyo3/maturin build --release -m /io/server/Cargo.toml
	@echo "Building for Linux aarch64..."
	docker run --rm --platform linux/arm64 -v $$(pwd):/io ghcr.io/pyo3/maturin build --release -m /io/server/Cargo.toml

# Build all wheels (local + Linux if Docker available)
build-wheels: build-wheels-local build-wheels-linux

# Upload wheels to PyPI
publish-pypi: $(MATURIN) build-wheels
	@if [ -z "$(INFTYAI_PYPI_TOKEN)" ]; then \
		echo "Error: INFTYAI_PYPI_TOKEN environment variable not set"; \
		exit 1; \
	fi
	@if [ ! -d "target/wheels" ] || [ -z "$$(ls -A target/wheels/*.whl 2>/dev/null)" ]; then \
		echo "Error: No wheels found. Run 'make build-wheels' first"; \
		exit 1; \
	fi
	@echo "Uploading wheels to PyPI..."
	@ls target/wheels/*.whl
	$(MATURIN) upload target/wheels/*.whl --skip-existing --username __token__ --password $(INFTYAI_PYPI_TOKEN)

.PHONY: publish-crate
# Publish daemon binary to crates.io
publish-crate:
	@echo "Publishing sandd daemon to crates.io..."
	cargo publish --package sandd
