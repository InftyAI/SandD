# SandD - Sandbox Daemon

Remote execution system for sandbox agents. Rust-powered server with Python API, designed for secure command execution in isolated environments.

## Features

- ✅ **Command Execution**: Execute shell commands remotely with timeout support
- ✅ **Interactive Shell (PTY)**: Full terminal sessions for debugging and manual work
- ✅ **File Transfer**: Upload/download files between agent and daemons
- ✅ **High Performance**: Rust-powered WebSocket server handles 200+ concurrent connections
- ✅ **Auto Reconnection**: Daemons automatically reconnect if connection drops
- ✅ **Heartbeat Monitoring**: Automatic stale connection cleanup
- ✅ **Cross-Platform**: Works on Linux, macOS, Windows

## Architecture

```
┌─────────────────────────────────────────┐
│  Python Agent Application               │
│  ┌────────────────────────────────────┐ │
│  │  from sandbox_execution import     │ │
│  │  Server                            │ │
│  │                                    │ │
│  │  server = Server("0.0.0.0", 8765)  │ │
│  │  result = server.execute_command(  │ │
│  │      "daemon-1", "ls -la"          │ │
│  │  )                                 │ │
│  └────────────────────────────────────┘ │
│          ▲                              │
│          │ Python bindings (PyO3)       │
│          ▼                              │
│  ┌────────────────────────────────────┐ │
│  │  Rust WebSocket Server (tokio)     │ │
│  │  • Command routing                 │ │
│  │  • Session management              │ │
│  └────────────────────────────────────┘ │
└─────────────────────────────────────────┘
              ▲
              │ WebSocket (WSS)
              │ (Daemon initiates connection)
              │
    ┌─────────┼─────────┐
    │         │         │
┌───▼───┐ ┌──▼────┐ ┌──▼────┐
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
result = server.execute_command("worker-1", "hostname")
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

## Usage Examples

### Command Execution

```python
from sandd import Server

server = Server("0.0.0.0", 8765)

# Simple command
result = server.execute_command("worker-1", "ls -la /tmp")
if result.success:
    print(result.stdout)
else:
    print(f"Failed: {result.stderr}")

# With environment variables
result = server.execute_command(
    "worker-1",
    "echo $MY_VAR",
    env={"MY_VAR": "custom_value"}
)

# With timeout and working directory
result = server.execute_command(
    "worker-1",
    "python long_script.py",
    timeout_secs=600,
    cwd="/opt/app"
)
```

### Interactive Shell

```python
# Start shell session
shell = server.start_shell("worker-1", rows=24, cols=80)

# Send commands
shell.write(b"cd /tmp\n")
shell.write(b"ls -la\n")

# Read output
import time
time.sleep(0.5)
output = shell.read(timeout=1.0)
if output:
    print(output.decode())

# Resize terminal
shell.resize(rows=50, cols=120)
```

### File Transfer

```python
# Upload file
with open("config.yaml", "rb") as f:
    data = f.read()
server.upload_file("worker-1", "/etc/app/config.yaml", data)

# Download file
data = server.download_file("worker-1", "/var/log/app.log")
with open("app.log", "wb") as f:
    f.write(data)
```

### Managing Daemons

```python
# List connected daemons
daemons = server.list_daemons()
print(f"Connected: {daemons}")

# Get statistics
stats = server.get_stats()
print(f"Total: {stats.total_daemons}")
print(f"By platform: {stats.by_platform}")
print(f"Oldest connection: {stats.oldest_connection_secs}s")

# Wait for specific daemon
if server.wait_for_daemon("worker-1", timeout=60):
    print("Daemon connected!")
```

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
