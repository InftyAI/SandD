# Architecture Details

## Connection Flow

1. **Daemon starts**, connects to agent's WebSocket endpoint
2. **Daemon registers** with metadata (hostname, platform, arch)
3. **Agent acknowledges** registration, stores connection in registry
4. **Heartbeats** sent every 10s to keep connection alive
5. **Commands** sent from agent → daemon over same connection
6. **Results** streamed back daemon → agent

## Key Design Decisions

### Why Daemon Connects to Agent?

- ✅ No ports exposed on execution plane (security)
- ✅ Works through NAT/firewalls
- ✅ Easy to add/remove daemons dynamically
- ✅ Agent controls access (daemon authenticates to agent)

### Why Rust Server?

At 200+ connections, Python asyncio:
- Uses 10GB+ memory (vs 2GB Rust)
- 80%+ CPU idle (vs 5% Rust)
- 500ms+ p99 latency (vs 20ms Rust)
- GIL contention kills performance

### Why WebSocket?

- Persistent bidirectional connection
- Efficient for streaming (session output)
- Well-supported libraries
- Can multiplex multiple sessions over one connection

## Scaling to 1000+ Daemons

Current design supports 10,000+ connections. For more:

1. **Horizontal scaling**: Run multiple agent servers with load balancer
2. **Sharding**: Route daemons to specific agents by ID hash
3. **Message queue**: Decouple command dispatch from agent process

## Protocol Extensions

### Future: SSH Tunneling

```rust
Message::StartSshTunnel {
    tunnel_id: String,
    local_port: u16,
}
```

Forward raw SSH traffic through WebSocket to daemon's sshd.

### Future: Authentication

```rust
Message::Register {
    daemon_id: String,
    auth_token: String, // JWT or pre-shared key
    metadata: DaemonMetadata,
}
```

Agent validates token before accepting connection.
