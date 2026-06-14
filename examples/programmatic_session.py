#!/usr/bin/env python3
"""
Example: Programmatic Session Control

Demonstrates how to use non-interactive sessions for:
- Multi-step command sequences
- Inspecting session output programmatically
- Handling session state and errors
- Building automation scripts
"""

import sys
import time
from sandd import Server


def main():
    # Start server
    server = Server("0.0.0.0", 8765)
    print(f"Server listening on {server.address}")

    # Wait for daemon
    daemon_id = "daemon-1"
    print(f"\nWaiting for daemon '{daemon_id}'...")
    if not server.wait_for_daemon(daemon_id, timeout=30):
        print(f"Daemon '{daemon_id}' did not connect")
        sys.exit(1)

    print(f"Daemon '{daemon_id}' connected!\n")

    # Create a non-interactive session
    print("=== Creating Session ===")
    session = server.new_session(daemon_id, rows=24, cols=80)
    print("Session created\n")

    # Example 1: Execute command and capture output
    print("=== Example 1: Basic Command ===")
    session.write(b"echo 'Hello from session'\n")
    time.sleep(0.2)
    output = session.read(timeout=1.0)
    if output:
        print(f"Output: {output.decode()}")

    # Example 2: Multi-step workflow
    print("\n=== Example 2: Multi-Step Workflow ===")
    steps = [
        ("mkdir -p /tmp/test", "Creating directory"),
        ("cd /tmp/test", "Changing directory"),
        ("pwd", "Verifying location"),
        ("echo 'test' > file.txt", "Creating file"),
        ("cat file.txt", "Reading file"),
    ]

    for cmd, description in steps:
        print(f"{description}: {cmd}")
        session.write(f"{cmd}\n".encode())
        time.sleep(0.1)
        output = session.read(timeout=1.0)
        if output:
            result = output.decode().strip()
            if result:
                print(f"  → {result}")

    # Example 3: Error handling
    print("\n=== Example 3: Error Handling ===")
    session.write(b"exit 42\n")  # Exit with non-zero code
    time.sleep(0.2)

    # Try to write after exit - should fail gracefully
    try:
        session.write(b"echo 'after exit'\n")
        output = session.read(timeout=1.0)
        if output:
            print(f"Output: {output.decode()}")
    except Exception as e:
        print(f"Session closed (expected): {e}")

    # Example 4: Create new session for long-running task
    print("\n=== Example 4: Long-Running Task ===")
    session2 = server.new_session(daemon_id)
    session2.write(b"for i in 1 2 3; do echo \"Step $i\"; sleep 1; done\n")

    # Stream output as it arrives
    start = time.time()
    while time.time() - start < 5:
        output = session2.read(timeout=0.5)
        if output:
            print(output.decode(), end='', flush=True)
        else:
            break

    session2.close()
    print("\n\nSession closed")


if __name__ == "__main__":
    main()
