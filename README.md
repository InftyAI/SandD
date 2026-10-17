<div align="center">

# SandD

**Sandbox Daemon for Agent Command Execution**

[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/python-3.8+-blue.svg)](https://www.python.org/)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Rust-powered WebSocket server with Python API for remote command execution and interactive sessions.

</div>

---

## Features

- **Command Execution** - Run shell commands on remote machines with timeout control
- **Interactive Sessions** - Full PTY sessions with bash for manual work
- **Snapshots** - Save, tag and restore workspace state on a worker
- **Labels & Broadcast** - Filter workers by label and run a command on many at once
- **Tunnel Mode (VPN)** - Secure mesh networking with WireGuard encryption via Tailscale
- **High Performance** - Rust async runtime handles high-concurrency workloads
- **Auto Reconnection** - Workers reconnect automatically on network failures
- **Cross-Platform** - Linux and macOS release binaries; Windows builds from source

## Architecture

```
┌──────────────────────────────────────────┐
│  Python Agent Application                │
│  ┌────────────────────────────────────┐  │
│  │  from sandd import Server          │  │
│  │                                    │  │
│  │  server = Server("0.0.0.0", 8765)  │  │
│  │  result = server.exec(             │  │
│  │      "daemon-1", "ls -la"          │  │
│  │  )                                 │  │
│  └────────────────────────────────────┘  │
│          ▲                               │
│          │ Python bindings (PyO3)        │
│          ▼                               │
│  ┌────────────────────────────────────┐  │
│  │  Rust WebSocket Server (tokio)     │  │
│  │  • Command routing                 │  │
│  │  • Session management              │  │
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
                     ▲
                     │ WebSocket
                     │ (ws:// in direct mode, encrypted via VPN in tunnel mode)
                     │
           ┌─────────┼─────────┐
           │         │         │
       ┌───▼───┐ ┌───▼───┐ ┌───▼───┐
       │Daemon │ │Daemon │ │Daemon │
       │  #1   │ │  #2   │ │  #n   │
       └───────┘ └───────┘ └───────┘
```

## Quick Start

Run a server and a daemon on one machine and execute a command. You need Rust
(https://rustup.rs) and Python 3.8+.

**1. Build both from source:**

```bash
git clone https://github.com/InftyAI/SandD
cd SandD
pip install .          # server: the `sandd` Python package
make daemon-release    # daemon: ./target/release/sandd
```

**2. Terminal 1, start the server:**

```python
from sandd import Server

server = Server()  # listens on 0.0.0.0:8765
print("waiting for worker-1...")
server.wait_for_daemon("worker-1", timeout=120)

result = server.exec("worker-1", "uname -a")
print(result.stdout)
```

**3. Terminal 2, start a daemon:**

```bash
./target/release/sandd --server-url ws://127.0.0.1:8765/ws --daemon-id worker-1
```

Terminal 1 prints the daemon's `uname -a`. On real machines, run the daemon on each
worker with `ws://<server-ip>:8765/ws`; workers dial out, so only the server's
port needs to be reachable.

## Installation

The server is a Python package (`from sandd import Server`); the daemon is a
separate binary, also named `sandd`, that runs on each worker.

### Server (Python package)

From PyPI:
```bash
pip install sandd
```

From source (needs Rust):
```bash
git clone https://github.com/InftyAI/SandD
cd SandD
pip install .
```

### Daemon (worker binary)

#### Quick Install

```bash
# Direct mode (no tunnel)
curl -fsSL https://raw.githubusercontent.com/InftyAI/SandD/main/hack/scripts/install.sh | sudo bash

# Tunnel mode (with Tailscale)
curl -fsSL https://raw.githubusercontent.com/InftyAI/SandD/main/hack/scripts/install.sh | sudo bash -s -- --tunnel
```

This installs the latest release to `/usr/local/bin/sandd`. To pin a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/InftyAI/SandD/main/hack/scripts/install.sh | sudo bash -s -- --version v0.2.0
```

#### Alternative Methods

**Install from crates.io:**
```bash
cargo install sandd
```

**Build from source:**
```bash
git clone https://github.com/InftyAI/SandD
cd SandD
make daemon-release              # binary at ./target/release/sandd
# or put it on your PATH (~/.cargo/bin/sandd):
cargo install --path sandd
```

**Build from the main branch:**
```bash
cargo install --git https://github.com/InftyAI/SandD sandd
```

## Usage

### Direct Mode (Development)

```python
from sandd import Server

server = Server()  # Direct mode (default)
server.wait_for_daemon("worker-1", timeout=30)

result = server.exec("worker-1", "hostname")
print(result.stdout)
```

```bash
sandd --server-url ws://server-ip:8765/ws --daemon-id worker-1
```

### Tunnel Mode (Production)

For secure multi-cloud deployments with mesh VPN (no TLS setup needed):

```python
from sandd import Server, TunnelConfig

config = TunnelConfig(
    authkey="YOUR_KEY",
    server="http://headscale:8080",
)
server = Server(connect="tunnel", tunnel_config=config)
# ✓ Encrypted with WireGuard (no TLS needed)
# ✓ Works across NAT/firewalls
# ✓ No public IPs required
```

```bash
sandd --server-url ws://10.200.0.1:8765/ws \
      --daemon-id worker-1 \
      --tunnel \
      --tunnel-authkey YOUR_KEY \
      --tunnel-server http://headscale:8080
```

In a cluster, don't bake a key into your image — mint one per start from a key
broker (`$SANDD_KEYBROKER_URL`) and take the headscale URL from
`$SANDD_TUNNEL_SERVER`:

```python
from sandd import Server, tunnel_config_from_env

server = Server(connect="tunnel", tunnel_config=tunnel_config_from_env())
```

See [Tunnel Mode Guide](./docs/proposals/TUNNEL.md) for setup instructions.

## Security

SandD does not authenticate daemons yet. The server accepts any client that
speaks the `sandd.v1` protocol, and a daemon can register under any id, so anyone who
can reach the server's port can join and receive commands meant for that id.

- **Direct mode:** run the server only on a trusted network.
- **Tunnel mode:** only nodes admitted to your Headscale mesh can reach the
  server, and traffic is encrypted with WireGuard. The `authkey` is a Headscale
  key for joining the mesh, not a SandD credential.

Daemon authentication and authorization with Keycloak is planned (see
[Architecture](./docs/ARCHITECTURE.md#future-authentication)).

## Documentation

- [Quick Start Guide](./docs/QUICKSTART.md)
- [Architecture Details](./docs/ARCHITECTURE.md)
- [Protocol Specification](./docs/proposals/PROTOCOL.md)
- [Tunnel Mode Guide](./docs/proposals/TUNNEL.md)
- [Development Guide](./docs/DEVELOP.md)
- [Examples](./examples)

## Roadmap

- [ ] **Authentication & Authorization** - Keycloak-issued tokens for daemons (see [Security](#security))
- [ ] **File Transfer** - Upload and download files between server and workers
- [ ] **Audit Logging** - Track all commands, sessions, and file transfers
- [ ] **Metrics** - Prometheus-compatible metrics for monitoring
- [ ] **Resource Limits** - CPU/memory/timeout controls per daemon
- [ ] **Multi-tenancy** - Isolated workspaces with access control
- [ ] **Rate Limiting** - Prevent abuse and resource exhaustion
- [ ] **Command Allowlist** - Restrict allowed commands per daemon

## Contributing

We welcome any kind of contributions, feedback, and suggestions! See [DEVELOP.md](./docs/DEVELOP.md) for development setup and guidelines.

## License

Apache-2.0 — see [LICENSE](./LICENSE). Third-party components (e.g. Tailscale, used by
tunnel mode) are listed in [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md).
