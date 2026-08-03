RUFF := .venv/bin/ruff
PYTEST := .venv/bin/pytest
MATURIN := .venv/bin/maturin

# Pinned so lint results don't shift when ruff changes its default rule set.
RUFF_VERSION := ruff==0.15.15

.PHONY: help build install dev test clean daemon-build daemon-release test-e2e test-e2e-tunnel docker-build docker-down

help:
	@echo "SandD - Sandbox Daemon - Build Commands"
	@echo ""
	@echo "  make build           - Build Python package (debug mode)"
	@echo "  make install         - Install Python package locally"
	@echo "  make dev             - Install in development mode with hot reload"
	@echo "  make test            - Run unit and integration tests (fast, no Docker)"
	@echo "  make test-e2e        - Run direct-mode end-to-end tests with Docker (slow)"
	@echo "  make test-e2e-tunnel - Run tunnel-mode (Tailscale mesh) e2e tests (slow)"
	@echo "  make daemon-build    - Build daemon binary (debug)"
	@echo "  make daemon-release  - Build daemon binary (release)"
	@echo "  make docker-build    - Build Docker image for daemon"
	@echo "  make docker-down     - Stop and remove Docker containers"
	@echo "  make clean           - Clean build artifacts"

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
	@echo "Running direct-mode E2E tests with Docker..."
	$(PYTEST) python/tests/ -m "e2e and not tunnel" -v -s
	@echo ""
	@echo "Cleaning up containers..."
	docker compose -f hack/docker/docker-compose.e2e.yml down

# Tunnel-mode e2e uses its OWN compose stack (headscale + mesh) and the test
# fixture mints the auth key mid-bringup, so it runs separately from test-e2e.
# The `tunnel` marker selects only these tests; the fixture handles up/down of
# docker-compose.tunnel-e2e.yml, but we `down` here too as a cleanup backstop.
test-e2e-tunnel: $(PYTEST) dev
	@echo "Building tunnel-mode Docker images..."
	docker compose -f hack/docker/docker-compose.tunnel-e2e.yml build
	@echo ""
	@echo "Running tunnel-mode E2E tests (Tailscale/headscale mesh)..."
	$(PYTEST) python/tests/ -m tunnel -v -s
	@echo ""
	@echo "Cleaning up containers..."
	docker compose -f hack/docker/docker-compose.tunnel-e2e.yml down -v

docker-build:
	docker compose -f hack/docker/docker-compose.e2e.yml build

docker-up:
	docker compose -f hack/docker/docker-compose.e2e.yml up -d

docker-down:
	docker compose -f hack/docker/docker-compose.e2e.yml down

.PHONY: lint
lint: $(RUFF)
	$(RUFF) check .

# `check --fix` exits non-zero when unfixable errors remain, which would stop
# make before the formatter runs -- hence the leading `-`. `make lint` is what
# gates on remaining errors.
.PHONY: format
format: $(RUFF)
	-$(RUFF) check --fix .
	$(RUFF) format .

$(RUFF):
	@echo "Installing ruff..."
	@python3 -m venv .venv || true
	@.venv/bin/pip install --quiet '$(RUFF_VERSION)'
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

# Publish one crate unless that exact version is already on crates.io. cargo has
# no --skip-existing, and crates.io is append-only, so reruns of a partially
# completed release would otherwise fail. Args: $(1) crate name, $(2) manifest dir,
# $(3) index path (see https://doc.rust-lang.org/cargo/reference/registry-index.html).
# grep -F keeps the dots in a version literal rather than regex wildcards; the
# trailing comma anchors the field so 0.0.1 can't match inside 0.0.10.
define publish_crate_once
	@VER=$$(grep -m1 '^version' $(2)/Cargo.toml | cut -d'"' -f2); \
	if curl -sf "https://index.crates.io/$(3)" 2>/dev/null \
	     | grep -qF "\"vers\":\"$$VER\","; then \
		echo "$(1) $$VER already on crates.io; skipping."; \
	else \
		echo "Publishing $(1) $$VER to crates.io..."; \
		cargo publish --package $(1); \
	fi
endef

.PHONY: publish-crate
# Publish the daemon to crates.io. sandd-protocol must go first: cargo strips the
# `path` dependency on publish and resolves it from the registry instead, so the
# protocol crate has to already be there.
publish-crate:
	$(call publish_crate_once,sandd-protocol,protocol,sa/nd/sandd-protocol)
	$(call publish_crate_once,sandd,sandd,sa/nd/sandd)

.PHONY: publish-crate-dry
# Same ordering, no uploads -- use this to validate manifests before a release.
publish-crate-dry:
	cargo publish --package sandd-protocol --dry-run
	@echo "NOTE: 'cargo publish --package sandd --dry-run' cannot succeed until"
	@echo "      sandd-protocol is actually on crates.io."

.PHONY: publish
# Publish everything: crates.io first, then PyPI.
publish: publish-crate publish-pypi

.PHONY: benchmark
benchmark: $(MATURIN)
	@echo "Running benchmarks for sandd daemon..."
	@echo ""
	cargo bench --package sandd

.PHONY: benchmark-results
benchmark-results:
	@echo "Benchmark results for sandd daemon:"
	@echo ""
	open target/criterion/report/index.html
