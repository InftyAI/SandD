# Quick Start Guide

## 1. Build Everything

```bash
# Install dependencies
pip3 install maturin

# Build and install
./test_build.sh
```

## 2. Terminal 1 - Start Agent

```python
# simple_agent.py
from sandd import Server
import time

server = Server("0.0.0.0", 8765)
print(f"Server running on {server.address}")
print("Waiting for daemons...")

while True:
    count = server.daemon_count()
    if count > 0:
        print(f"\n{count} daemon(s) connected:")
        for daemon_id in server.list_daemons():
            result = server.execute_command(daemon_id, "hostname")
            print(f"  {daemon_id}: {result.stdout.strip()}")
    time.sleep(5)
```

Run: `python3 simple_agent.py`

## 3. Terminal 2+ - Start Daemons

```bash
./target/release/sandd \
    --server-url ws://localhost:8765/ws \
    --daemon-id my-daemon-1
```

Start 200+ on different machines pointing to same agent URL.

## 4. Test

```python
from sandd import Server

server = Server()
server.wait_for_daemon("my-daemon-1", timeout=30)

result = server.execute_command("my-daemon-1", "uname -a")
print(result.stdout)
```

Done!
