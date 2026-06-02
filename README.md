<div align="center">

# SandD

**A Lightweight Sandbox Daemon that Provides Secure, Isolated Execution Environments for Agents.**

[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/python-3.8+-blue.svg)](https://www.python.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Rust-powered WebSocket server with Python API for secure command execution in isolated environments.

[Features](#features) • [Quick Start](#quick-start) • [Architecture](#architecture) • [Documentation](./docs)

</div>

---

## Features

- ✅ **Command Execution**: Execute commands remotely with timeout support
- ✅ **Interactive Sessions (PTY)**: Full terminal sessions for debugging and manual work
- ✅ **File Transfer**: Upload/download files between agent and daemons
- ✅ **High Performance**: Rust-powered WebSocket server handles 200+ concurrent connections
- ✅ **Auto Reconnection**: Daemons automatically reconnect if connection drops
- ✅ **Heartbeat Monitoring**: Automatic stale connection cleanup
- ✅ **Cross-Platform**: Works on Linux, macOS, Windows

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

**Key Design**: Daemons connect **TO** the agent (not the other way around), so no ports need to be exposed on the execution plane.

## Quick Start

### 1. Build the System

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build Python package
make install

# Build daemon binary
make daemon-release
```

### 2. Start the Agent (Python)

```python
from sandd import Server

# Start server
server = Server(host="0.0.0.0", port=8765)
print(f"Server listening on {server.address}")

# Wait for daemons
server.wait_for_daemon("worker-1", timeout=30)

# Execute command
result = server.exec("worker-1", "hostname")
print(f"Output: {result.stdout}")
```

### 3. Start Daemons (Remote Machines)

```bash
# On remote machine 1
./target/release/sandd \
    --server-url ws://agent-host:8765/ws \
    --daemon-id worker-1

# On remote machine 2
./target/release/sandd \
    --server-url ws://agent-host:8765/ws \
    --daemon-id worker-2

# Or let it auto-generate a UUID
./target/release/sandd \
    --server-url ws://agent-host:8765/ws

# ... repeat for n+ machines
```

## Examples

See the [examples/](./examples) directory for common use cases.

## Development

See [DEVELOP.md](./docs/DEVELOP.md) for the complete developer guide including build commands, testing, and troubleshooting.

## Security Considerations

1. **No exposed daemon ports**: Daemons only make outbound connections to the agent
2. **Authentication**: Add token-based auth in production (not included in MVP)
3. **TLS/WSS**: Use `wss://` in production for encrypted connections
4. **Sandboxing**: Consider running daemon in containers or VMs
5. **Command validation**: Validate/sanitize commands in your application

## Future Enhancements

- [ ] SSH protocol tunneling (for IDE remote development)
- [ ] Token-based authentication
- [ ] Command audit logging
- [ ] Resource limits per daemon
- [ ] Metrics/monitoring integration (Prometheus)
- [ ] Multi-tenancy support
- [ ] Command history and replay

## License

MIT

## Contributing

Issues and PRs welcome! This is a production-ready foundation for remote command execution.
