# Tunnel Mode Overview

SandD supports secure tunnel mode for production deployments using mesh VPN technology.

## When to Use Tunnel Mode

**Use tunnel mode when:**
- Deploying across multiple clouds (AWS + GCP + Azure)
- Controller should not be publicly accessible
- Need automatic NAT traversal
- Want network-level isolation

**Use direct mode when:**
- Single datacenter / trusted network
- Development and testing
- Quick prototyping

---

## Direct Mode vs Tunnel Mode (VPN)

### Visual Comparison

**Direct Mode (No VPN):**
```
┌──────────┐                        ┌──────────┐
│ Daemon   │──── WebSocket over ───→│Controller│
│          │     public internet    │Public IP │
└──────────┘                        └──────────┘

- Direct WebSocket connection
- No VPN
- Controller needs public IP
- Daemons connect over internet
```

**Tunnel Mode (Mesh VPN):**
```
┌──────────┐                        ┌──────────┐
│ Daemon   │════ VPN tunnel ════════│Controller│
│ Mesh IP  │  WireGuard encrypted   │ Mesh IP  │
└──────────┘                        └──────────┘
     ↓                                   ↓
  Join VPN                            Join VPN
     ↓                                   ↓
┌────────────────────────────────────────────┐
│      Headscale (VPN coordinator)           │
└────────────────────────────────────────────┘

- VPN mesh network
- Encrypted tunnels between nodes
- Private mesh IPs
- No public IPs needed
```

### Feature Comparison

| Feature | Direct Mode | Tunnel Mode (VPN) |
|---------|-------------|-------------------|
| **Setup complexity** | Simple (5 min) | Medium (15 min) |
| **Controller IP** | Must be public | Can be private |
| **Daemon location** | Anywhere (outbound) | Anywhere (mesh) |
| **NAT traversal** | Manual (firewall rules) | Automatic (hole punching) |
| **Encryption** | Need to add TLS | Built-in (WireGuard) |
| **Port exposure** | Public (attack surface) | Hidden (mesh only) |
| **Multi-cloud** | Need VPC peering | Works automatically |
| **Use case** | Single cloud/datacenter | Cross-cloud, laptop↔cloud |

### When to Use Each

**Use Direct Mode when:**
- ✅ Controller has stable public IP
- ✅ Single cloud or trusted network
- ✅ Development and testing
- ✅ Simple setup preferred

**Use Tunnel Mode (VPN) when:**
- ✅ Controller behind NAT (laptop, home, corporate)
- ✅ Multiple clouds (AWS + GCP + Azure)
- ✅ Don't want exposed ports
- ✅ Need encrypted communication
- ✅ Dynamic IPs or ephemeral instances

---

## How Tunnel Mode Works

### The Problem: NAT and Private Networks

**Why you can't connect directly:**

```
Laptop (Controller)          Cloud VM (Daemon)
Private: 192.168.1.100       Private: 10.0.1.20
Behind home router           Behind cloud firewall

❌ Can't reach each other's private IPs
❌ Need to expose public ports (security risk)
❌ Need VPN peering between networks (complex)
```

### The Solution: Four Components Working Together

**Secure mesh requires ALL four pieces:**

```
┌────────────────────────────────────────┐
│ 1. Coordination (Headscale)            │
│    "Who can join? Where are they?"     │
│    → Authentication & peer discovery   │
└────────────────────────────────────────┘
                 +
┌────────────────────────────────────────┐
│ 2. NAT Traversal (Hole Punching)       │
│    "How do I reach you behind NAT?"    │
│    → Makes devices reachable           │
└────────────────────────────────────────┘
                 +
┌────────────────────────────────────────┐
│ 3. Encryption (WireGuard)              │
│    "How do I protect the data?"        │
│    → Confidentiality & integrity       │
└────────────────────────────────────────┘
                 +
┌────────────────────────────────────────┐
│ 4. Identity (Cryptographic Keys)       │
│    "How do I verify who you are?"      │
│    → Node authentication               │
└────────────────────────────────────────┘
                 =
         Secure Mesh Network
```

**What each component does:**

| Component | Problem Solved | Without It |
|-----------|----------------|------------|
| **Headscale** | Who's allowed? Where are peers? | Can't find each other |
| **Hole Punching** | How to reach through NAT? | Can't connect |
| **WireGuard** | How to protect data? | Traffic readable |
| **Keys** | How to verify identity? | Anyone can impersonate |

### Step-by-Step: How Connection Happens

**1. Both sides connect OUT to Headscale**

```
┌──────────────────────────────┐
│ Headscale (Public)           │
│ 203.0.113.100:8080           │
└──────────────────────────────┘
     ↑                    ↑
     │ Outbound ✓         │ Outbound ✓
     │ (firewalls allow)  │
┌────┴─────┐         ┌────┴─────┐
│ Laptop   │         │ Cloud VM │
│ NAT hole │         │ NAT hole │
│ created  │         │ created  │
└──────────┘         └──────────┘
```

**2. Headscale learns each node's "hole"**

```
Laptop connects → Headscale sees: 203.0.113.50:60001
VM connects → Headscale sees: 198.51.100.25:41234

Headscale tells each about the other:
→ Laptop: "VM is at 198.51.100.25:41234"
→ VM: "Laptop is at 203.0.113.50:60001"
```

**3. Nodes punch holes simultaneously**

```
Both send packets at same time:
→ Laptop sends to VM's address
→ VM sends to Laptop's address

NATs see outbound packets, allow replies
Result: Direct encrypted tunnel! ✓
```

**4. WireGuard encrypts all traffic**

```
Every packet encrypted with:
- ChaCha20-Poly1305 (cipher)
- Curve25519 (key exchange)
- Authentication tags

Even if intercepted: unreadable gibberish
```

---

## Architecture

### Components

**Headscale (Server)**
- Coordination server for mesh network
- Runs separately (single instance for entire mesh)
- Issues keys, manages peer discovery

**Tailscale Client**
- VPN client that connects to Headscale
- Runs in each container (installed via `hack/docker/Dockerfile.server-tunnel`)
- Joins the mesh, creates tunnel interface

**Your Application = Controller**
- When you call `Server()`, you ARE the controller
- It starts a WebSocket server that daemons connect to
- In tunnel mode, your app needs Tailscale to join the mesh

### Direct Mode
```
Daemon → Internet → Controller (public IP:8765)
```

### Tunnel Mode
```
┌─────────────────────────────────────────────────────────┐
│                    Headscale Server                     │
│                  (runs once, centrally)                 │
└─────────────────────────────────────────────────────────┘
           ↑                                   ↑
           │                                   │
┌──────────▼──────────────────────┐  ┌─────────▼─────────┐
│  Your Application (Controller)  │  │  Daemon           │
│                                 │  │  (worker)         │
│  Server() starts WebSocket srv  │  │                   │
│  (Tailscale client)             │  │ (Tailscale client)│
│  10.200.0.1                     │  │  10.200.0.2       │
└─────────────────────────────────┘  └───────────────────┘
                   Private Mesh Network

```

**Key:** `hack/docker/Dockerfile.server-tunnel` installs **Tailscale client** (not Headscale server). Headscale runs separately.

---

## Using Tunnel Mode

### In Your Application

```python
from sandd import Server, TunnelConfig

# Direct mode (default)
server = Server()

# Tunnel mode
config = TunnelConfig(
    authkey="your-headscale-preauth-key",
    server="http://headscale:8080"
)
server = Server(connect="tunnel", tunnel_config=config)
```

### Docker Image

Use the tunnel-enabled image. Build it yourself like this:
```bash
docker build -f hack/docker/Dockerfile.server-tunnel -t my-app:tunnel .
```

### Running

```bash
# Your app code contains TunnelConfig with auth key and server URL
docker run \
  --cap-add NET_ADMIN \
  --device /dev/net/tun \
  my-app:tunnel
```

---

## Setup Steps

### 1. Build Tunnel Image

```bash
# From SandD repo
docker build -f hack/docker/Dockerfile.server-tunnel -t inftyai/sandd-server-tunnel:latest .
```

### 2. Run Headscale

```bash
docker run -d \
  -p 8080:8080 \
  -v headscale-data:/var/lib/headscale \
  headscale/headscale:latest serve
```

### 3. Generate Auth Keys

```bash
# Create user
docker exec headscale headscale users create sandd

# Generate keys (save this!)
docker exec headscale headscale preauthkeys create --user sandd --expiration 24h
# Output: key-abc123def456...
```

### 4. Write Your Controller App

```python
# controller.py
from sandd import Server, TunnelConfig
import time

config = TunnelConfig(
    authkey="key-abc123def456",  # From step 3
    server="http://headscale:8080"
)

server = Server(connect="tunnel", tunnel_config=config)
print("Controller ready, waiting for daemons...")

while True:
    daemons = server.list_daemons()
    print(f"Connected: {len(daemons)}")
    time.sleep(5)
```

### 5. Run Your Controller

```bash
docker run \
  --cap-add NET_ADMIN \
  --device /dev/net/tun \
  -v $(pwd)/controller.py:/app/controller.py \
  inftyai/sandd-server-tunnel:latest \
  python /app/controller.py
```

---

## Complete Example

See [examples/tunnel-simple/](../examples/tunnel-simple/) for a working docker-compose setup.

```bash
cd examples/tunnel-simple
docker-compose up
```

---

## Communication Flow

### Controller (Your App)
1. Container launches
2. `Server(connect="tunnel", tunnel_config=config)` called
3. Controller automatically starts Tailscale and joins mesh
4. Gets mesh IP (10.200.0.1)
5. WebSocket server starts on 10.200.0.1:8765

### Daemon
1. Run with `--tunnel` flag
2. `sandd` automatically starts Tailscale and joins mesh
3. Gets mesh IP (10.200.0.2)
4. Connects to controller at ws://10.200.0.1:8765/ws
5. Ready to execute commands

**One command:**
```bash
sandd --server-url ws://10.200.0.1:8765/ws \
      --daemon-id worker-1 \
      --tunnel \
      --tunnel-authkey YOUR_KEY \
      --tunnel-server http://headscale:8080
```

## Security Model

### What's Protected

**✅ Data in Transit**
- All traffic encrypted with WireGuard
- ChaCha20-Poly1305 cipher (military-grade)
- Perfect forward secrecy

**✅ Authentication**
- Pre-auth keys control mesh access
- Public key cryptography (Curve25519)
- Each node has unique identity

**✅ Network Isolation**
- Ports not exposed to internet
- Only mesh nodes can communicate
- Automatic NAT traversal (no manual firewall rules)

### Key Types and Security

**1. Auth Key (Pre-Auth Key)**

```bash
# Single-use (recommended)
headscale preauthkeys create --user sandd --expiration 1h

# Each node gets unique key
# Expires after first use
```

**If leaked:** Attacker can join mesh ❌

**Protection:**
- Use single-use keys
- Short expiration (1-24h)
- Rotate regularly
- Never commit to git

**2. WireGuard Private Key**

**Stored:** `/var/lib/tailscale/tailscaled.state`

**If leaked:** Attacker can decrypt all traffic to/from that node ❌

**Protection:**
```bash
# File permissions
chmod 600 /var/lib/tailscale/tailscaled.state

# Docker: use named volumes
volumes:
  - tailscale-state:/var/lib/tailscale
```

**3. Shared Secret**

**How it works:** Computed from your private key + peer's public key

**Security:** Never transmitted, only exists in RAM ✓

### Comparison with Other Approaches

| Security Aspect | Plain ws:// | wss:// (TLS) | Tailscale |
|----------------|-------------|--------------|-----------|
| **Encryption** | ❌ None | ✅ TLS 1.3 | ✅ WireGuard |
| **Authentication** | Manual | SSL certs | ✅ Built-in |
| **Port exposure** | ❌ Public | ❌ Public | ✅ Hidden |
| **NAT traversal** | Manual | Manual | ✅ Automatic |
| **Setup complexity** | Simple | Medium (certs) | Medium (Headscale) |
| **Zero-trust** | ❌ | ⚠️ CA-based | ✅ Crypto keys |

### Attack Scenarios and Mitigations

**Scenario 1: Auth Key Leaked**

```
Impact: Attacker joins mesh, accesses services

Mitigation:
1. Revoke compromised key
   headscale preauthkeys expire --prefix tskey-abc

2. Remove unauthorized nodes
   headscale nodes list
   headscale nodes delete --identifier <id>

3. Generate new keys
4. Update all legitimate nodes
```

**Scenario 2: Node Compromised (Root Access)**

```
Impact: Attacker steals WireGuard key, decrypts traffic

Mitigation:
1. Remove node from mesh
   headscale nodes delete --identifier <id>

2. Delete state file on node
   rm -rf /var/lib/tailscale/tailscaled.state

3. Investigate compromise
4. Rejoin with new keys
```

**Scenario 3: Headscale Server Compromised**

```
Impact:
- Can see who's connected (metadata)
- Cannot decrypt traffic (end-to-end encrypted)

Mitigation:
- Headscale doesn't store private keys
- Data never decrypted at coordinator
- Limit: Can kick nodes off, but can't read data
```

### Best Practices

**Key Management:**
```bash
# ✅ DO: Single-use, short-lived
headscale preauthkeys create --expiration 1h

# ❌ DON'T: Reusable, long-lived
headscale preauthkeys create --reusable --expiration 8760h
```

**Secrets Storage:**
```bash
# ✅ DO: Use secrets management
export KEY=$(vault read -field=key secret/sandd)

# ❌ DON'T: Hardcode in files
SANDD_TUNNEL_AUTH_KEY=tskey-abc123  # Never commit!
```

**Monitoring:**
```bash
# Check for unauthorized nodes
headscale nodes list --output json | \
  jq '.[] | select(.created > "2024-01-01")'
```

---

## FAQ

**Q: Is hole punching safe?**
A: Yes. Hole punching only finds the network path. All data is encrypted with WireGuard. Think of it like finding a road (hole punching) vs using an armored truck (encryption).

**Q: Why not just use WebSocket with TLS?**
A: WebSocket needs a public IP and open ports. Tailscale works when controller is behind NAT (laptop, private cloud) and provides automatic encryption.

**Q: Can Headscale read my data?**
A: No. Headscale only coordinates connections. Data is encrypted end-to-end between nodes. Headscale never sees decrypted traffic.

**Q: What if my auth key leaks?**
A: Attacker can join your mesh. Use single-use keys and revoke immediately if leaked. See Security Model section.

**Q: Why not install Headscale in my container?**
A: Headscale is a coordination server - you only need one for the entire mesh. Like DNS: one server, many clients.

**Q: What's in `hack/docker/Dockerfile.server-tunnel`?**
A: Python 3.11, SandD library, and Tailscale client (not Headscale server).

**Q: Do I need NET_ADMIN?**
A: Yes. VPN requires `--cap-add NET_ADMIN --device /dev/net/tun`

---

## Troubleshooting

### Check tunnel status

```bash
# Inside container
docker exec <container> tailscale status
docker exec <container> tailscale ip
```

### Permission denied

Ensure container has required capabilities:
```bash
--cap-add NET_ADMIN --device /dev/net/tun
```

### Can't reach controller

Verify mesh IP:
```bash
docker exec controller tailscale ip -4
# Use this IP in CONTROLLER_URL
```

---

## Next Steps

- [Detailed Setup Guide](./networking/headscale.md)
- [Configuration Reference](./networking/configuration.md)
- [Kubernetes Deployment](./deployment/kubernetes.md) (coming soon)
