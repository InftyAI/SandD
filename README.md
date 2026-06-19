<div align="center">

# SandD

**Sandbox Daemon for Agent Command Execution**

[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/python-3.8+-blue.svg)](https://www.python.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Rust-powered WebSocket server with Python API for remote command execution and interactive sessions.

</div>

---

## Features

- **Command Execution** - Run shell commands on remote machines with timeout control
- **Interactive Sessions** - Full PTY sessions with bash for manual work
- **File Transfer** - Upload/download files between controller and workers
- **High Performance** - Rust async runtime handles high-concurrency workloads
- **Auto Reconnection** - Workers reconnect automatically on network failures
- **Cross-Platform** - Linux, macOS, Windows support

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
                     │ WebSocket (WSS)
                     │ (Daemon initiates connection)
                     │
           ┌─────────┼─────────┐
           │         │         │
       ┌───▼───┐ ┌───▼───┐ ┌───▼───┐
       │Daemon │ │Daemon │ │Daemon │
       │  #1   │ │  #2   │ │  #n   │
       └───────┘ └───────┘ └───────┘
```

## Installation

### Python Package (Controller)

Install from PyPI:
```bash
pip install sandd
```

Or build from source:
```bash
git clone https://github.com/InftyAI/SandD
cd SandD
make install
```

### Daemon Binary (Worker)

#### Quick Install

```bash
# Direct mode (no tunnel)
curl -fsSL https://get.sandd.dev/install.sh | sudo bash

# Tunnel mode (with Tailscale)
curl -fsSL https://get.sandd.dev/install.sh | sudo bash -s -- --tunnel
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
make daemon-release
# Binary at: ./target/release/sandd
```

## Quick Start

### Direct Mode (Development)

**Start controller:**

```python
from sandd import Server

server = Server()  # Direct mode (default)
server.wait_for_daemon("worker-1", timeout=30)

result = server.exec("worker-1", "hostname")
print(result.stdout)
```

**Start daemon:**

```bash
# Direct mode
sandd --server-url ws://controller-ip:8765/ws --daemon-id worker-1

# Tunnel mode
sandd --server-url ws://10.200.0.1:8765/ws \
      --daemon-id worker-1 \
      --tunnel \
      --tunnel-authkey YOUR_KEY \
      --tunnel-server http://headscale:8080
```

### Tunnel Mode (Production)

For secure multi-cloud deployments with mesh VPN:

```python
from sandd import Server

server = Server(connect="tunnel")  # Secure tunnel mode
```

See [Tunnel Mode Guide](./docs/TUNNEL.md) for setup instructions.

## Documentation

- [Quick Start Guide](./docs/QUICKSTART.md)
- [Architecture Details](./docs/ARCHITECTURE.md)
- [Protocol Specification](./docs/PROTOCOL.md)
- [Development Guide](./docs/DEVELOP.md)
- [Examples](./examples)

## Security

⚠️ **Add security layers for production use:**

- Use `wss://` (TLS) instead of plain `ws://`
- Add authentication (tokens, mTLS)
- Run workers in containers
- Validate commands before execution
- Audit log all commands

## Roadmap

- [ ] **Authentication** - Token-based auth for daemon connections
- [ ] **TLS Support** - Built-in WSS with certificate management
- [ ] **Audit Logging** - Track all commands, sessions, and file transfers
- [ ] **Metrics** - Prometheus-compatible metrics for monitoring
- [ ] **Resource Limits** - CPU/memory/timeout controls per daemon
- [ ] **Multi-tenancy** - Isolated workspaces with access control
- [ ] **Rate Limiting** - Prevent abuse and resource exhaustion
- [ ] **Command Allowlist** - Restrict allowed commands per daemon

## Contributing

We welcome any kind of contributions, feedback, and suggestions! See [DEVELOP.md](./docs/DEVELOP.md) for development setup and guidelines.

## License

MIT
