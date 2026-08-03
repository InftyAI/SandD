# Docker Files

This directory contains Docker-related files for building and testing SandD.

## Files

### Dockerfiles

#### Server (Controller) Images

- **`Dockerfile.server-tunnel`** - Server with Tailscale (build from source)
  - Use: Development and testing
  - Build: `docker build -f hack/docker/Dockerfile.server-tunnel -t inftyai/sandd-server:latest-tunnel .`
  - See: [docs/proposals/TUNNEL.md](../../docs/proposals/TUNNEL.md)

- **`Dockerfile.server-tunnel-release`** - Server with Tailscale (uses PyPI release)
  - Use: Production deployments
  - Build: `docker build -f hack/docker/Dockerfile.server-tunnel-release --build-arg SANDD_VERSION=0.1.0 -t inftyai/sandd-server:v0.1.0-tunnel .`

#### Daemon (Worker) Images

- **`Dockerfile.daemon-tunnel`** - Daemon with Tailscale (build from source)
  - Use: Development and testing
  - Build: `docker build -f hack/docker/Dockerfile.daemon-tunnel -t inftyai/sandd-daemon:latest-tunnel .`

- **`Dockerfile.daemon-tunnel-release`** - Daemon with Tailscale (uses GitHub release)
  - Use: Production deployments
  - Build: `docker build -f hack/docker/Dockerfile.daemon-tunnel-release --build-arg SANDD_VERSION=v0.1.0 -t inftyai/sandd-daemon:v0.1.0-tunnel .`

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

### Build tunnel-enabled image

```bash
# From repo root
docker build -f hack/docker/Dockerfile.server-tunnel -t inftyai/sandd-server:latest-tunnel .
```

### Build test images

```bash
docker build -f hack/docker/Dockerfile.debian -t inftyai/sandd-daemon:debian .
docker build -f hack/docker/Dockerfile.alpine -t inftyai/sandd-daemon:alpine .
docker build -f hack/docker/Dockerfile.rocky -t inftyai/sandd-daemon:rocky .
```
