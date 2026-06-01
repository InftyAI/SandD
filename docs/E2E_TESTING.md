# End-to-End Testing with Docker

This guide explains how to run E2E tests for SandD using Docker containers.

## Overview

The E2E test suite runs multiple daemon instances in Docker containers that connect to a Python agent server running on the host machine. This simulates a real distributed environment.

## Architecture

```
┌─────────────────────────────┐
│  Host Machine               │
│  ┌────────────────────────┐ │
│  │  Python Test Suite     │ │
│  │  (pytest)              │ │
│  │                        │ │
│  │  Server: 0.0.0.0:8765  │ │
│  └────────┬───────────────┘ │
│           │                 │
└───────────┼─────────────────┘
            │ WebSocket
     ┌──────┼──────┬──────────┐
     │      │      │          │
┌────▼───┐ ┌▼─────▼┐  ┌──────▼──┐
│Daemon-1│ │Daemon-2│  │Daemon-3 │
│Container│ │Container│ │Container│
│env=test│ │env=test│  │env=prod │
│us-east │ │us-west │  │eu-west  │
└────────┘ └────────┘  └─────────┘
```

## Prerequisites

1. Docker and Docker Compose installed
2. Python development environment set up
3. Project built with `make dev`

## Running E2E Tests

### Quick Start

```bash
# Run all E2E tests
make test-e2e
```

This command will:
1. Build Docker images for the daemon
2. Start 3 daemon containers
3. Run the E2E test suite
4. Clean up containers

### Manual Steps

```bash
# Build Docker images
make docker-build

# Start containers
docker compose -f docker-compose.e2e.yml up -d

# Run tests
.venv/bin/pytest python/tests/test_e2e.py -v -s

# Stop containers
make docker-down
```

## Test Coverage

The E2E test suite covers:

### Basic Operations
- **Connection**: All 3 daemons connect successfully
- **Command Execution**: Execute commands on each daemon
- **Concurrent Execution**: Run commands simultaneously on multiple daemons

### Label-Based Filtering
- Filter daemons by `env` label (test/prod)
- Filter daemons by `region` label (us-east, us-west, eu-west)

### File Transfer
- Upload files to daemon containers
- Download files from daemon containers
- Cross-container file operations

### Resilience
- Daemon reconnection after container restart
- Connection stability under load

### Statistics
- Server stats reflect all connected daemons
- Platform detection for containerized daemons

## Test Fixtures

### `docker_daemons` (module scope)
Starts and manages Docker containers for the test session.

### `server` (module scope)
Creates a Server instance and waits for all daemons to connect.

## Configuration

### Docker Compose

The `docker-compose.e2e.yml` defines 3 daemon containers:

| Container | ID | Labels |
|-----------|-----|--------|
| sandd-daemon-1 | daemon-1 | env=test, region=us-east |
| sandd-daemon-2 | daemon-2 | env=test, region=us-west |
| sandd-daemon-3 | daemon-3 | env=prod, region=eu-west |

### Network Configuration

Daemons use `host.docker.internal` to connect to the host machine's server. This works on:
- Docker Desktop (Mac/Windows)
- Linux with `extra_hosts` configuration

## Troubleshooting

### Daemons not connecting

```bash
# Check container logs
docker logs sandd-daemon-1

# Check if containers are running
docker ps | grep sandd

# Test connectivity from container
docker exec sandd-daemon-1 ping host.docker.internal
```

### Port conflicts

If port 8765 is in use:
1. Stop other services using that port
2. Or modify the port in `docker-compose.e2e.yml` and `test_e2e.py`

### Build failures

```bash
# Clean and rebuild
make clean
docker compose -f docker-compose.e2e.yml build --no-cache
```

### Tests hanging

```bash
# Force stop containers
docker compose -f docker-compose.e2e.yml down -v

# Check for zombie processes
ps aux | grep sandd
```

## CI/CD Integration

For GitHub Actions or other CI systems:

```yaml
- name: Run E2E Tests
  run: |
    make dev
    make test-e2e
```

## Performance

- **Build time**: ~2-3 minutes (first build)
- **Test duration**: ~30-60 seconds
- **Cleanup time**: ~5 seconds

## Advanced Usage

### Running specific test classes

```bash
.venv/bin/pytest python/tests/test_e2e.py::TestE2ELabels -v
```

### Running with more daemons

Modify `docker-compose.e2e.yml` to add more daemon services, then update the test fixtures accordingly.

### Debug mode

```bash
# Keep containers running after tests
docker compose -f docker-compose.e2e.yml up -d
.venv/bin/pytest python/tests/test_e2e.py -v -s --pdb
# Containers stay up for debugging
```

## Security Considerations

E2E tests use insecure WebSocket (`ws://`) for simplicity. Production deployments should use:
- WSS (WebSocket Secure) with TLS
- Authentication tokens
- Network policies
- Resource limits

## Future Enhancements

- [ ] Add stress tests with 50+ containers
- [ ] Test network failures and reconnection
- [ ] Test TLS/WSS connections
- [ ] Add performance benchmarks
- [ ] Test resource limits enforcement
