# Developer Guide

This guide is for developers working on SandD itself.

## Prerequisites

- **Rust**: Install from https://rustup.rs/
- **Python 3.8+**: With pip
- **Docker**: Only for the e2e tests and images

The Makefile creates `.venv/` and installs maturin, pytest and ruff into it as needed.

## Project Structure

```
SandD/
├── protocol/         # Shared message types (sandd-protocol crate)
│   └── src/lib.rs
│
├── server/           # Server: Rust WebSocket server + PyO3 bindings
│   ├── src/
│   │   ├── lib.rs       # Crate root
│   │   ├── server.rs    # WebSocket server (axum): /ws, /stats, /health
│   │   ├── registry.rs  # Daemon connection registry
│   │   └── python.rs    # Python API bindings (behind the `python` feature)
│   └── Cargo.toml
│
├── sandd/            # Daemon binary (runs on workers)
│   ├── src/
│   │   ├── main.rs      # Entry point, connection loop, tunnel setup
│   │   ├── executor.rs  # Command execution
│   │   ├── session.rs   # Interactive sessions (PTY)
│   │   └── snapshot/    # Workspace snapshots
│   ├── benches/         # Criterion benchmarks
│   └── Cargo.toml
│
├── python/
│   ├── sandd/        # Python package (Server, models, tunnel keys; AsyncServer is a stub)
│   └── tests/        # Unit, integration and e2e tests
│
├── hack/             # Dockerfiles, compose stacks, install script
└── examples/         # Usage examples
```

## Building

```bash
make dev              # Build and install the Python package (debug, editable)
make daemon-build     # Build the daemon (debug): ./target/debug/sandd

make release          # Python package (optimized)
make daemon-release   # Daemon binary (optimized): ./target/release/sandd
```

Run `make help` for the full list of targets.

The `python` feature of `server/` is off by default, so plain `cargo build` and
`cargo test` work without a Python interpreter. Maturin turns it on when building the
wheel (see `[tool.maturin]` in `pyproject.toml`).

## Development Workflow

### 1. Make Changes

Edit files in `protocol/src/`, `server/src/`, `sandd/src/`, or `python/sandd/`.

### 2. Rebuild

```bash
# If you changed protocol/, server/ or python/sandd/
make dev

# If you changed protocol/ or sandd/
make daemon-build
```

### 3. Try It

```bash
# Terminal 1: start a server
python3 -c "
from sandd import Server
server = Server()
server.wait_for_daemon('dev-1', timeout=60)
print(server.exec('dev-1', 'uname -a').stdout)
"

# Terminal 2: start a daemon
./target/debug/sandd --server-url ws://127.0.0.1:8765/ws --daemon-id dev-1
```

See [examples/](../examples) for sessions, snapshots and tunnel mode.

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

```bash
make test             # Lint + Rust tests + Python tests (no Docker)
make test-e2e         # Direct-mode e2e against daemons in Docker (slow)
make test-e2e-tunnel  # Tunnel-mode e2e with a Headscale mesh in Docker (slow)
make lint             # ruff check
make format           # ruff fix + format
make benchmark        # Daemon benchmarks (criterion)
```

Python tests are selected by marker: `e2e` for Docker-based tests, `tunnel` for the
tunnel stack. `make test` runs `-m "not e2e"`.

## Common Tasks

### Adding a New Message Type

1. Add the variant to `Message` in `protocol/src/lib.rs` (shared by server and daemon):
   ```rust
   MyNewCommand { field: String }
   ```

2. Handle it in `server/src/server.rs`:
   ```rust
   Message::MyNewCommand { field } => {
       // Forward to daemon or handle
   }
   ```

3. Handle it in `sandd/src/main.rs`:
   ```rust
   Message::MyNewCommand { field } => {
       // Execute and respond
   }
   ```

### Adding Python API

1. Add a method to `Server` in `server/src/python.rs`:
   ```rust
   #[pymethods]
   impl Server {
       fn my_method(&self, arg: String) -> PyResult<()> {
           // Implementation
       }
   }
   ```

2. Add a wrapper in `python/sandd/server.py`:
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

**Server logs:** `Server(verbose=True)` (the default) logs at INFO; set
`RUST_LOG=sandbox_server=debug` for more.

## Known Issues & Limitations

1. **File transfer is not supported yet.** `Server.upload_file` and
   `Server.download_file` exist, and the protocol messages are defined, but the
   feature is unfinished: don't rely on either call.
2. **No daemon authentication.** Any client that speaks `sandd.v1` can register as any
   daemon id. Run the server on a trusted network or in tunnel mode. See
   [ARCHITECTURE.md](ARCHITECTURE.md#future-authentication).

### Warnings

- PyO3 `non_local_definitions` warning: Safe to ignore (PyO3 macro limitation)

## Performance

At 200 concurrent daemons:
- Memory: ~2-3 GB
- CPU (idle): ~5%
- CPU (100 cmds/sec): ~15-25%
- Command latency p99: <20ms

## Contributing

### Before Submitting PR

1. Run `make test`
2. Check formatting: `cargo fmt --all` and `make format`
3. Check lints: `cargo clippy --workspace --all-targets`
4. Update docs if adding features

### Commit Style

```
feat: brief description

More detailed explanation if needed.
Include motivation and context.
```

Use a `feat:`, `fix:` or `chore:` prefix, as in the existing history.

### Adding Dependencies

- Keep dependencies minimal
- Prefer well-maintained crates
- Check licensing compatibility (Apache-2.0)

## Release Process

1. Update the version in `pyproject.toml`, `protocol/Cargo.toml`, `server/Cargo.toml`
   and `sandd/Cargo.toml` (and the `sandd-protocol` dependency version in
   `sandd/Cargo.toml`)
2. Tag and push: `git tag v0.x.y && git push upstream v0.x.y`. The `Release` workflow
   builds the daemon binaries and creates the GitHub release.
3. Publish packages: `make publish` (wheels to PyPI, needs `INFTYAI_PYPI_TOKEN`; crates
   to crates.io)

## Troubleshooting

### User Issues

**Daemon won't connect:**
- Check the server is up: `curl http://server-host:8765/health`
- Verify firewall allows outbound WebSocket connections
- Check daemon logs: `RUST_LOG=info ./target/release/sandd ...`
- Check what the server sees: `curl http://server-host:8765/stats`

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
- Check [QUICKSTART.md](QUICKSTART.md) for usage examples
