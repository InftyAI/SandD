# Developer Guide

This guide is for developers working on SandD itself.

## Prerequisites

- **Rust**: Install from https://rustup.rs/
- **Python 3.8+**: With pip
- **Maturin**: `pip install maturin`

## Project Structure

```
SandD/
├── server/           # Rust server with PyO3 bindings
│   ├── src/
│   │   ├── lib.rs       # Python API bindings
│   │   ├── server.rs    # WebSocket server (axum)
│   │   ├── registry.rs  # Daemon connection registry
│   │   └── protocol.rs  # Message protocol
│   └── Cargo.toml
│
├── sandd/            # Rust daemon binary
│   ├── src/
│   │   ├── main.rs      # Daemon entry point
│   │   ├── executor.rs  # Command execution
│   │   ├── session.rs   # Interactive sessions (PTY)
│   │   └── protocol.rs  # Message protocol
│   └── Cargo.toml
│
├── python/sandd/     # Python package wrapper
│   └── __init__.py
│
└── examples/         # Usage examples
```

## Building

### Quick Build (Development)

```bash
# Build everything
./test_build.sh

# Or step by step:
make dev              # Build Python package (debug)
make daemon-build     # Build daemon (debug)
```

### Release Build

```bash
make release          # Python package (optimized)
make daemon-release   # Daemon binary (optimized)
```

### Manual Build

```bash
# Python package
maturin develop --release -m server/Cargo.toml

# Daemon
cargo build --package sandd --release
```

## Development Workflow

### 1. Make Changes

Edit files in `server/src/`, `sandd/src/`, or `python/sandd/`

### 2. Rebuild

```bash
# If you changed server/
make dev

# If you changed daemon/
make daemon-build

# If you changed Python wrapper only
# (no rebuild needed, it's just a wrapper)
```

### 3. Test

```bash
# Terminal 1: Start test agent
python3 examples/simple_test.py

# Terminal 2: Start daemon
./target/debug/sandd --server-url ws://127.0.0.1:8765/ws
```

### 4. Run Examples

```bash
python3 examples/agent_example.py
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed design.

**Key concepts:**

### Channel-Based Communication

```
Python → Registry → Channel → handle_websocket → WebSocket → Daemon
         (cmd_tx)    (bridge)  (cmd_rx)          (network)
```

Why channels? No WebSocket type conflicts, lock-free, idiomatic async Rust.

### Message Flow

**Outgoing (Python → Daemon):**
```rust
command_tx: mpsc::UnboundedSender<Message>  // Stored in registry
```

**Incoming (Daemon → Python):**
```rust
pending_commands: oneshot::Sender<Result>    // Request/Response
sessions: mpsc::Sender<Vec<u8>>              // Streaming
file_transfers: Vec<Vec<u8>>                 // Chunked buffering
```

## Testing

### Unit Tests

```bash
cargo test --workspace
```

### Integration Tests

```bash
pytest tests/  # (when tests are added)
```

### Manual Testing

Use `examples/agent_example.py` to test all features.

## Common Tasks

### Adding a New Command Type

1. Add to `protocol.rs` (both server and daemon):
   ```rust
   Message::MyNewCommand { field: String }
   ```

2. Handle in `server/src/server.rs`:
   ```rust
   Message::MyNewCommand { field } => {
       // Forward to daemon or handle
   }
   ```

3. Handle in `sandd/src/main.rs`:
   ```rust
   Message::MyNewCommand { field } => {
       // Execute and respond
   }
   ```

### Adding Python API

1. Add method to `Server` in `server/src/lib.rs`:
   ```rust
   #[pymethods]
   impl Server {
       fn my_method(&self, arg: String) -> PyResult<()> {
           // Implementation
       }
   }
   ```

2. Add wrapper in `python/sandd/__init__.py`:
   ```python
   def my_method(self, arg: str) -> None:
       """User-friendly docstring"""
       self._server.my_method(arg)
   ```

### Debugging

**Enable Rust logs:**
```bash
RUST_LOG=debug ./target/debug/sandd --server-url ws://127.0.0.1:8765/ws
```

**Python side:**
```python
import logging
logging.basicConfig(level=logging.DEBUG)
```

**Check WebSocket traffic:**
```bash
# In server
RUST_LOG=server=debug python3 examples/simple_test.py
```

## Known Issues & Limitations (MVP)

### Not Implemented

1. **File Transfer**: Protocol defined, daemon just logs
   - Reason: Deferred for MVP
   - Fix: Implement actual file I/O in daemon

### Warnings

- PyO3 `non_local_definitions` warning: Safe to ignore (PyO3 macro limitation)
- Unused imports/variables: Run `cargo fix` to clean up

## Performance

At 200 concurrent daemons:
- Memory: ~2-3 GB
- CPU (idle): ~5%
- CPU (100 cmds/sec): ~15-25%
- Command latency p99: <20ms

## Contributing

### Before Submitting PR

1. Run tests: `cargo test --workspace`
2. Check formatting: `cargo fmt --all`
3. Check lints: `cargo clippy --all`
4. Test manually with examples
5. Update docs if adding features

### Commit Style

```
Add feature: brief description

More detailed explanation if needed.
Include motivation and context.
```

### Adding Dependencies

- Keep dependencies minimal
- Prefer well-maintained crates
- Check licensing compatibility (MIT)

## Release Process

1. Update version in `pyproject.toml` and `Cargo.toml` files
2. Update `CHANGELOG.md` (when added)
3. Build release: `make release && make daemon-release`
4. Test release build
5. Tag: `git tag v0.x.0`
6. Build wheel: `maturin build --release -m server/Cargo.toml`
7. Publish: `maturin publish` (when ready)

## Troubleshooting

### User Issues

**Daemon won't connect:**
- Check agent URL is reachable: `curl -v ws://agent-host:8765/ws`
- Verify firewall allows outbound WebSocket connections
- Check agent server is running: `ps aux | grep python`
- Check daemon logs: `RUST_LOG=info ./target/release/sandd ...`

**Commands timing out:**
- Increase `timeout` parameter in `exec()` (in seconds)
- Check daemon system resources: `top`, `free -h`
- Verify command actually completes when run manually
- Check daemon logs for errors

**High memory usage:**
- Monitor active sessions (they hold state)
- Close unused sessions with `session.close()`
- Check number of connected daemons: `server.daemon_count()`

### Development Issues

### "no reactor running" panic

**Symptom:** `PanicException: there is no reactor running`

**Cause:** Trying to use `tokio::runtime::Handle::current()` from Python thread

**Fix:** Store runtime handle in the struct, use it for `block_on()`

### Type mismatch with WebSocket

**Symptom:** `expected tokio_tungstenite::WebSocket, found axum::WebSocket`

**Solution:** Use channels, don't store WebSocket directly in registry

### Maturin "missing field `package`"

**Symptom:** Maturin can't find package in workspace

**Fix:** Use `-m server/Cargo.toml` flag

## Protocol

WebSocket-based JSON protocol for agent-daemon communication.

For complete protocol specification, see [proposals/PROTOCOL.md](proposals/PROTOCOL.md).

## Resources

- [PyO3 Guide](https://pyo3.rs/)
- [Tokio Docs](https://tokio.rs/)
- [Axum Docs](https://docs.rs/axum/)
- [Rust Async Book](https://rust-lang.github.io/async-book/)

## Questions?

- Check [ARCHITECTURE.md](ARCHITECTURE.md) for design details
- Check [proposals/PROTOCOL.md](proposals/PROTOCOL.md) for protocol specification
- Check [STATUS.md](STATUS.md) for implementation status
- Check [QUICKSTART.md](QUICKSTART.md) for usage examples
