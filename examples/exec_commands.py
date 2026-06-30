#!/usr/bin/env python3
"""Minimal script for executing simple commands on daemons"""

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

        print(
            f"\rConnected: {stats.total_daemons} | Platforms: {stats.by_platform}",
            end="",
            flush=True,
        )

        if daemons and len(daemons) > 0:
            for daemon in daemons:
                daemon_id = daemon.id
                try:
                    # Test 1: Python script
                    result = server.exec(
                        daemon_id,
                        "python3 -c 'import sys; print(f\"Python {sys.version_info.major}.{sys.version_info.minor}\")'",
                        timeout=5,
                    )
                    if result.success:
                        print(
                            f"\n✓ Python test passed on {daemon_id}: {result.stdout.strip()}"
                        )
                    else:
                        print(
                            f"\n✗ Python test failed on {daemon_id}: exit_code={result.exit_code}"
                        )

                    # Test 2: Wrong Python script (intentional error)
                    result = server.exec(
                        daemon_id, "python3 -c 'undefined_variable'", timeout=5
                    )
                    if not result.success:
                        print(
                            f"✓ Error handling test passed on {daemon_id}, stderr: {result.stderr.strip()}"
                        )
                    else:
                        print(
                            f"✗ Error handling test failed on {daemon_id}: expected error but got success"
                        )

                    # Test 3: Echo command
                    result = server.exec(
                        daemon_id, "echo 'Hello from daemon!'", timeout=5
                    )
                    if result.success:
                        print(
                            f"✓ Echo test passed on {daemon_id}: {result.stdout.strip()}"
                        )

                except Exception as e:
                    print(f"\n✗ Command failed on {daemon_id}: {e}")

        time.sleep(2)
except KeyboardInterrupt:
    print("\n\nShutting down...")
    sys.exit(0)
