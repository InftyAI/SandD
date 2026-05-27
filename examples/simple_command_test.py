#!/usr/bin/env python3
"""Minimal test script for SandD"""

from sandd import Server
import time
import sys

print("Starting SandD server...")
server = Server("127.0.0.1", 8765)
print(f"✓ Server started on {server.address}")
print("\nStart a daemon with:")
print("  ./target/release/sandd --server-url ws://127.0.0.1:8765/ws")
print("\nWaiting for daemons (Ctrl+C to exit)...\n")

try:
    while True:
        daemons = server.list_daemons()
        stats = server.get_stats()

        print(f"\rConnected: {stats.total_daemons} | Platforms: {stats.by_platform}", end="", flush=True)

        if daemons and len(daemons) > 0:
            for daemon_id in daemons:
                try:
                    result = server.execute_command(daemon_id, "echo test", timeout=5)
                    if result.success:
                        print(f"\n✓ Command test passed on {daemon_id}: {result.stdout.strip()}")
                except Exception as e:
                    print(f"\n✗ Command failed: {e}")

        time.sleep(2)
except KeyboardInterrupt:
    print("\n\nShutting down...")
    sys.exit(0)
