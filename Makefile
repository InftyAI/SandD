RUFF := .venv/bin/ruff
PYTEST := .venv/bin/pytest
MATURIN := .venv/bin/maturin

# Pinned so lint results don't shift when ruff changes its default rule set.
RUFF_VERSION := ruff==0.15.15

.PHONY: help build install dev test clean daemon-build daemon-release controller-build controller-release test-e2e test-e2e-tunnel docker-build docker-down

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
	@echo "  make controller-build   - Build controller binary (debug)"
	@echo "  make controller-release - Build controller binary (release)"
	@echo "  make docker-build    - Build Docker image for daemon"
	@echo "  make docker-down     - Stop and remove Docker containers"
	@echo "  make clean           - Clean build artifacts"
	@echo ""
	@echo "Controller image (native Rust binary — what Nebula runs):"
	@echo "  make docker-build-controller        - Build both arches (no push)"
	@echo "  make docker-build-controller-local  - Build host arch only, load locally"
	@echo "  make docker-push-controller         - Build both arches and push a manifest"
	@echo ""
	@echo "Tunnel server (Python-hosted controller) image — multi-arch (amd64 + arm64):"
	@echo "  make docker-build-server-tunnel        - Build both arches (no push)"
	@echo "  make docker-build-server-tunnel-local  - Build host arch only, load locally"
	@echo "  make docker-push-server-tunnel         - Build both arches and push a manifest"

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
	@echo "Running Rust tests (controller: lib + binary)..."
	# NOT --lib: that skips src/main.rs, where the controller's config/flag rules and
	# the "auth on but material missing must be FATAL" tests live. --no-default-features
	# is implicit (python is off by default), which is also what the bin target needs.
	cargo test --package sandbox-server --lib --bins
	@echo ""
	@echo "Running Python tests (excluding e2e)..."
	$(PYTEST) python/tests/ -m "not e2e"

daemon-build:
	cargo build --package sandd

daemon-release:
	cargo build --package sandd --release
	@echo ""
	@echo "SandD binary built at: ./target/release/sandd"

# The CONTROLLER binary — what Nebula runs, one Deployment per workload. No
# --features python: the pyo3 layer must stay out or the bin cannot link at all
# (extension-module leaves the CPython symbols undefined). See server/Cargo.toml.
controller-build:
	cargo build --package sandbox-server --bin sandd-controller

controller-release:
	cargo build --package sandbox-server --bin sandd-controller --release
	@echo ""
	@echo "SandD controller binary built at: ./target/release/sandd-controller"

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

# --- Controller image (native Rust binary) -------------------------------------
#
# For running the controller STANDALONE. Nebula does not pull this: it links the
# controller into its own manager through the C ABI (server/src/ffi.rs), so there is
# no controller Deployment and no image to pin. Nothing publishes this image either
# — the release workflow ships the daemon binaries only.
#
# Distroless + one static-ish binary, ~50MB against the ~4GB server-tunnel image
# below, because it carries no interpreter, no rustup and no Tailscale client.
#
# Multi-arch for the same reason as every other image here (see the note below):
# built on arm64 Macs, deployed to mostly-amd64 nodes.
CONTROLLER_IMG ?= inftyai/sandd-controller
CONTROLLER_TAG ?= latest

.PHONY: docker-build-controller
docker-build-controller: buildx-builder
	docker buildx build \
		--builder $(BUILDX_BUILDER) \
		--platform $(PLATFORMS) \
		-f hack/docker/Dockerfile.controller \
		-t $(CONTROLLER_IMG):$(CONTROLLER_TAG) \
		.

# Host arch only, loaded into the local docker store so it can actually be run
# (`docker run --rm $(CONTROLLER_IMG):$(CONTROLLER_TAG) --help`). A multi-platform
# build cannot be --load'ed: the local store holds one arch per tag.
.PHONY: docker-build-controller-local
docker-build-controller-local: buildx-builder
	docker buildx build \
		--builder $(BUILDX_BUILDER) \
		-f hack/docker/Dockerfile.controller \
		-t $(CONTROLLER_IMG):$(CONTROLLER_TAG) \
		--load \
		.

.PHONY: docker-push-controller
docker-push-controller: buildx-builder
	docker buildx build \
		--builder $(BUILDX_BUILDER) \
		--platform $(PLATFORMS) \
		-f hack/docker/Dockerfile.controller \
		-t $(CONTROLLER_IMG):$(CONTROLLER_TAG) \
		--push \
		.
	@echo ""
	@echo "Pushed $(CONTROLLER_IMG):$(CONTROLLER_TAG) for $(PLATFORMS)"
	@echo "Verify the manifest lists both arches:"
	@echo "  docker buildx imagetools inspect $(CONTROLLER_IMG):$(CONTROLLER_TAG)"

# --- Tunnel server (controller) image ------------------------------------------
#
# The PYTHON-hosted controller, kept for the tunnel/mesh e2e stacks and for driving a
# controller from Python (`from sandd import Server`). Nebula uses the native binary
# above instead — no Python drives its controllers and it does not use the mesh.
#
# The controller image must run on BOTH architectures: it is developed on arm64
# Macs but deployed to clusters whose nodes are usually amd64 (and increasingly
# arm64, e.g. Graviton). A single-arch image built on the dev machine dies on the
# node with `exec format error`, so these targets always build a multi-arch
# MANIFEST rather than whatever the host happens to be.
#
# Dockerfile.server-tunnel itself needs no arch handling: rustup and Tailscale's
# install.sh both detect the target at runtime, and maturin compiles for the
# build platform — so the same Dockerfile yields a correct image per platform.
SERVER_TUNNEL_IMG ?= inftyai/sandd-server-tunnel
SERVER_TUNNEL_TAG ?= latest
# Both platforms by default. Override to shorten a dev loop, e.g.
# `make docker-build-server-tunnel PLATFORMS=linux/arm64`.
PLATFORMS ?= linux/amd64,linux/arm64
# A named builder is REQUIRED for multi-platform work: the default `docker`
# driver can only build for the host platform. Created on demand, reused after.
BUILDX_BUILDER ?= sandd-multiarch

.PHONY: buildx-builder
buildx-builder:
	@docker buildx inspect $(BUILDX_BUILDER) >/dev/null 2>&1 || { \
		echo "Creating buildx builder $(BUILDX_BUILDER)..."; \
		docker buildx create --name $(BUILDX_BUILDER) --driver docker-container --bootstrap; \
	}

# Build both arches WITHOUT pushing, as a pre-flight check. Note the images stay
# in the build cache only: a multi-platform build cannot be loaded into the local
# docker image store (it holds one arch per tag), which is why there is no
# --load here. Use docker-build-server-tunnel-local to get a runnable image.
.PHONY: docker-build-server-tunnel
docker-build-server-tunnel: buildx-builder
	docker buildx build \
		--builder $(BUILDX_BUILDER) \
		--platform $(PLATFORMS) \
		-f hack/docker/Dockerfile.server-tunnel \
		-t $(SERVER_TUNNEL_IMG):$(SERVER_TUNNEL_TAG) \
		.

# Build for the HOST arch only and load it into the local docker store, so it can
# actually be run/inspected (`docker run ... python -c 'import sandd'`).
.PHONY: docker-build-server-tunnel-local
docker-build-server-tunnel-local: buildx-builder
	docker buildx build \
		--builder $(BUILDX_BUILDER) \
		-f hack/docker/Dockerfile.server-tunnel \
		-t $(SERVER_TUNNEL_IMG):$(SERVER_TUNNEL_TAG) \
		--load \
		.

# Build both arches and push as ONE multi-arch manifest, so a node pulling the tag
# gets its own architecture automatically. --push (not `docker push`) is required:
# the multi-arch result never lands in the local store, it goes straight to the
# registry. Requires `docker login` with push rights on $(SERVER_TUNNEL_IMG).
.PHONY: docker-push-server-tunnel
docker-push-server-tunnel: buildx-builder
	docker buildx build \
		--builder $(BUILDX_BUILDER) \
		--platform $(PLATFORMS) \
		-f hack/docker/Dockerfile.server-tunnel \
		-t $(SERVER_TUNNEL_IMG):$(SERVER_TUNNEL_TAG) \
		--push \
		.
	@echo ""
	@echo "Pushed $(SERVER_TUNNEL_IMG):$(SERVER_TUNNEL_TAG) for $(PLATFORMS)"
	@echo "Verify the manifest lists both arches:"
	@echo "  docker buildx imagetools inspect $(SERVER_TUNNEL_IMG):$(SERVER_TUNNEL_TAG)"

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
