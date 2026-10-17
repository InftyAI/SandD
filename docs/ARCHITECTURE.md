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
- ✅ Agent decides what runs (daemons only execute what the agent sends)

### Why Rust Server?

For high-concurrency workloads, Python asyncio:
- Uses significantly more memory than Rust
- Higher CPU usage and GIL contention
- Higher p99 latency
- Rust provides better performance and resource efficiency

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

Today the controller admits any client that speaks `sandd.v1`, and a daemon can
register under any id. Run it on a trusted network, or in tunnel mode, where only mesh
members can reach it.

The plan is to use Keycloak for authentication and authorization:

- The daemon gets an access token from Keycloak and sends it as
  `Authorization: Bearer <token>` on the WebSocket upgrade.
- The controller verifies the token against Keycloak's JWKS endpoint before upgrading,
  and rejects a bad one with HTTP 401.
- The token's claims decide which daemon id the connection registers as, and what it
  is allowed to do.
