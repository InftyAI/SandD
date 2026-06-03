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
            result = server.exec(daemon_id, "hostname")
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

Start many daemons on different machines pointing to same agent URL.

## 4. Test

```python
from sandd import Server

server = Server()
server.wait_for_daemon("my-daemon-1", timeout=30)

result = server.exec("my-daemon-1", "uname -a")
print(result.stdout)
```

Done!

---

## Usage Examples

### Command Execution

```python
from sandd import Server

server = Server("0.0.0.0", 8765)

# Simple command
result = server.exec("worker-1", "ls -la /tmp")
if result.success:
    print(result.stdout)
else:
    print(f"Failed: {result.stderr}")

# With environment variables
result = server.exec(
    "worker-1",
    "echo $MY_VAR",
    env={"MY_VAR": "custom_value"}
)

# With timeout and working directory
result = server.exec(
    "worker-1",
    "python long_script.py",
    timeout=600,
    cwd="/opt/app"
)
```

### Interactive Session

```python
# Start session
session = server.new_session("worker-1", rows=24, cols=80)

# Send commands
session.write(b"cd /tmp\n")
session.write(b"ls -la\n")

# Read output
import time
time.sleep(0.5)
output = session.read(timeout=1.0)
if output:
    print(output.decode())

# Resize terminal
session.resize(rows=50, cols=120)

# Close session
session.close()
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
