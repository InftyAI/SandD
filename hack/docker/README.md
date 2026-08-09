# Docker Files

This directory contains Docker-related files for building and testing SandD.

## Files

### Dockerfiles

#### Server (Controller) Images

- **`Dockerfile.server-tunnel`** - Server with Tailscale (build from source)
  - Use: Development and testing
  - Build: `make docker-build-server-tunnel` (amd64 + arm64; see "Multi-arch" below)
  - See: [docs/proposals/TUNNEL.md](../../docs/proposals/TUNNEL.md)

- **`Dockerfile.server-tunnel-release`** - Server with Tailscale (uses PyPI release)
  - Use: Production deployments
  - Build: `docker build -f hack/docker/Dockerfile.server-tunnel-release --build-arg SANDD_VERSION=0.1.0 -t inftyai/sandd-server-tunnel:v0.1.0 .`

#### Daemon (Worker) Images

- **`Dockerfile.daemon-tunnel`** - Daemon with Tailscale (build from source)
  - Use: Development and testing
  - Build: `docker build -f hack/docker/Dockerfile.daemon-tunnel -t inftyai/sandd-tunnel:latest .`

- **`Dockerfile.daemon-tunnel-release`** - Daemon with Tailscale (uses GitHub release)
  - Use: Production deployments
  - Build: `docker build -f hack/docker/Dockerfile.daemon-tunnel-release --build-arg SANDD_VERSION=v0.1.0 -t inftyai/sandd-tunnel:v0.1.0 .`

#### Test Images (Direct Mode)

- **`Dockerfile.debian`** - Debian-based daemon (for testing)
- **`Dockerfile.alpine`** - Alpine-based daemon (for testing)
- **`Dockerfile.rocky`** - Rocky Linux-based daemon (for testing)

### Docker Compose

- **`docker-compose.e2e.yml`** - End-to-end testing setup
  - Runs controller + multiple daemons (Debian, Alpine, Rocky)
  - Used by: `python/tests/test_e2e.py`
  - Run: `docker compose -f hack/docker/docker-compose.e2e.yml up`

## Building

### Build tunnel-enabled image (multi-arch)

The controller image must run on **both** `linux/amd64` and `linux/arm64`: it is
typically built on an arm64 Mac but deployed to cluster nodes that are usually amd64
(and sometimes arm64, e.g. Graviton). A plain `docker build` produces a **single-arch**
image for the host, which fails on a node of the other arch with `exec format error`.

Use the Makefile targets, which always build a multi-arch manifest:

```bash
# From repo root. Builds both arches, no push — a pre-flight check.
make docker-build-server-tunnel

# Build both arches and push ONE manifest, so each node pulls its own arch.
# Requires `docker login` with push rights on inftyai/.
make docker-push-server-tunnel

# Confirm the pushed manifest really lists both arches:
docker buildx imagetools inspect inftyai/sandd-server-tunnel:latest
```

Overridable variables: `SERVER_TUNNEL_IMG`, `SERVER_TUNNEL_TAG`, `PLATFORMS`,
`BUILDX_BUILDER`. For example, to push a versioned tag to your own registry:

```bash
make docker-push-server-tunnel \
  SERVER_TUNNEL_IMG=myrepo/sandd-server-tunnel SERVER_TUNNEL_TAG=v0.1.0
```

To iterate locally you need a **runnable** image, which a multi-platform build cannot
produce (the local docker store holds one arch per tag, so `--load` is incompatible
with two platforms). Build host-arch-only instead:

```bash
make docker-build-server-tunnel-local
docker run --rm inftyai/sandd-server-tunnel:latest \
  python -c "from sandd import Server, tunnel_config_from_env; print('ok')"
```

Each platform compiles its own native wheel (`maturin` produces e.g.
`manylinux_2_34_aarch64` and `..._x86_64`), and buildx runs both concurrently, so a
cold two-platform build is a few minutes rather than the hours emulated Rust builds
can imply. Pass `PLATFORMS=linux/arm64` (or `linux/amd64`) to halve it anyway.

### Build test images

```bash
docker build -f hack/docker/Dockerfile.debian -t inftyai/sandd-daemon:debian .
docker build -f hack/docker/Dockerfile.alpine -t inftyai/sandd-daemon:alpine .
docker build -f hack/docker/Dockerfile.rocky -t inftyai/sandd-daemon:rocky .
```
