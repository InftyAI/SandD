# Tunnel Mode Example

⚠️ **For Development/Testing Only** - See [production guide](../../docs/proposals/TUNNEL.md) for real deployments.

This example demonstrates tunnel mode setup with Headscale. It shows how to run the controller; you'll launch daemons separately.

---

## What's Included

This example provides a **complete working setup**:
- **Headscale** - Coordination server (assigns mesh IPs)
- **Controller** - Your app running `Server()` (WebSocket server)
- **Daemon** - Worker that executes commands
- **Config** - Minimal Headscale configuration

---

## Architecture

```
┌──────────────────────────────────────────────┐
│ Headscale (Coordination Server)              │
│ Image: headscale/headscale:0.23.0           │
│ Assigns IPs from 100.64.0.0/24             │
└──────────────────────────────────────────────┘
           ↑                         ↑
           │                         │
┌──────────┴─────────────┐   ┌──────┴────────────────┐
│ Controller (in example) │   │ Daemon (you run this) │
│                         │   │                       │
│ Server() in Python      │   │ sandd binary          │
│ Tailscale client        │   │ Tailscale client      │
│ Mesh IP: 100.64.0.1     │   │ Mesh IP: 100.64.0.2   │
│ Listens: :8765          │   │ Connects to 100.64.0.1│
└─────────────────────────┘   └───────────────────────┘
         Private Mesh Network
```

---

## Quick Start

### 1. (Optional) Build Images Manually

You can either:
- **Option A:** Let docker-compose build automatically (recommended for quick start)
- **Option B:** Build images manually first (useful for testing builds)

```bash
# Option B: Build manually from repo root
docker build -f hack/docker/Dockerfile.tunnel -t inftyai/sandd-server:latest-tunnel .
docker build -f hack/docker/Dockerfile.debian -t inftyai/sandd-daemon:debian .
```

**Note:** If you skip this step, docker-compose will build images automatically when you run `docker-compose up`.

### 2. Start ONLY Headscale (Not App Yet!)

```bash
cd examples/tunnel-simple

# Start only Headscale first
docker-compose up -d headscale

# Wait for it to be ready
sleep 2
```

### 3. Create Headscale User and Auth Key

```bash
# Create user
docker exec tunnel-simple-headscale-1 headscale users create sandd

# Generate REUSABLE pre-auth key (SAVE THIS!)
# --reusable is required because both controller and daemon use the same key
# DO NOT enable this in production but generate a new key for each daemon in production.
docker exec tunnel-simple-headscale-1 headscale preauthkeys create \
  --user sandd \
  --expiration 24h \
  --reusable

# Example output:
# key-abc123def456...
```

### 4. Start Everything with Auth Key

```bash
# Export the key from step 3
export SANDD_TUNNEL_AUTH_KEY=key-abc123def456

# Start controller + daemon (builds images if not already built)
docker-compose up -d

# Watch logs
docker-compose logs -f
```

**What happens:**
- Controller joins mesh → gets `100.64.0.1`
- Daemon joins mesh → gets `100.64.0.2`
- Daemon connects to controller automatically
- Controller shows "Connected daemons: 1"

### 5. Test the Connection

```bash
# See all services
docker-compose ps

# Check controller logs
docker-compose logs app

# Check daemon logs
docker-compose logs daemon

# Should see successful connection messages!
```

---

## Running Additional Daemons

Want to add more workers? Run manually:

```bash
# Use the SAME auth key from step 3
sandd --server-url ws://100.64.0.1:8765/ws \
      --daemon-id worker-1 \
      --tunnel \
      --tunnel-authkey key-abc123def456 \
      --tunnel-server http://localhost:8080
```

**What happens:**
1. Daemon starts Tailscale and joins mesh → gets `100.64.0.2`
2. Connects to controller at `ws://100.64.0.1:8765/ws`
3. Controller sees daemon and can send commands


---

## Configuration

### Where Does 100.64.0.0/24 Come From?

Open **headscale-config.yaml**:

```yaml
ip_prefixes:
  - 100.64.0.0/24    # ← Define mesh IP range here
```

When clients join:
- Controller → `100.64.0.1` (first client)
- Daemon 1 → `100.64.0.2` (second client)
- Daemon 2 → `100.64.0.3` (third client)

**You can change this to any private range** (e.g., `100.64.0.0/16`).

SandD doesn't hardcode IPs - it queries: `tailscale ip -4` to get the assigned mesh IP.

---

## Running Multiple Daemons

```bash
# Daemon 1 (gets 100.64.0.2)
sandd --server-url ws://100.64.0.1:8765/ws \
      --daemon-id worker-1 \
      --tunnel \
      --tunnel-authkey key-abc123 \
      --tunnel-server http://localhost:8080

# Daemon 2 (gets 100.64.0.3)
sandd --server-url ws://100.64.0.1:8765/ws \
      --daemon-id worker-2 \
      --tunnel \
      --tunnel-authkey key-abc123 \
      --tunnel-server http://localhost:8080
```

**All daemons use the same auth key** (from step 3).

---

## For Your Own App

### Dockerfile

```dockerfile
FROM inftyai/sandd-server:latest-tunnel

COPY my_controller.py .
CMD ["python", "my_controller.py"]
```

### Controller Code

```python
# my_controller.py
from sandd import Server, TunnelConfig
import os
import time

config = TunnelConfig(
    authkey=os.environ["TUNNEL_AUTH_KEY"],
    server=os.environ["TUNNEL_SERVER"]
)

server = Server(connect="tunnel", tunnel_config=config)
print("Controller ready at mesh IP")

# Your logic
while True:
    daemons = server.list_daemons()
    print(f"Connected: {len(daemons)} daemons")

    for daemon in daemons:
        result = server.exec(daemon.id, "hostname")
        print(f"{daemon.id}: {result.stdout.strip()}")

    time.sleep(10)
```

### Run

```bash
docker run \
  --cap-add NET_ADMIN \
  --device /dev/net/tun \
  -e TUNNEL_AUTH_KEY=key-abc123 \
  -e TUNNEL_SERVER=http://headscale:8080 \
  my-controller
```

---

## Cleanup

```bash
# Stop services
docker-compose down

# Remove data
docker-compose down -v
```

---

## Troubleshooting

### Controller doesn't get mesh IP

```bash
# Check Tailscale status inside controller
docker exec tunnel-simple-app-1 tailscale status
docker exec tunnel-simple-app-1 tailscale ip -4
```

### Daemon can't connect

```bash
# Verify controller mesh IP
docker exec tunnel-simple-app-1 tailscale ip -4

# Use that IP in daemon's --server-url
sandd --server-url ws://<controller-ip>:8765/ws ...
```

### Check Headscale logs

```bash
docker logs tunnel-simple-headscale-1
```

---

## Next Steps

- [Full Tunnel Guide](../../docs/proposals/TUNNEL.md)
- [Kubernetes Deployment](../../docs/deployment/kubernetes.md) (coming soon)
- [Production Best Practices](../../docs/deployment/production.md) (coming soon)
