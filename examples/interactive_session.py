#!/usr/bin/env python3
"""Example: Interactive session

This example demonstrates the interactive mode where you can
type commands directly in a live terminal session.

Usage:
    python examples/interactive_session.py
    make docker-up to start the server
"""

from sandd import Server
import sys
import time


def main():
    print("Interactive Session Example")
    print("=" * 50)

    # Connect to server, disable verbose logging for cleaner output
    server = Server("127.0.0.1", 8765, verbose=False)
    print(f"✓ Server started on {server.address}\n")

    # Wait for at least one daemon
    print("Waiting for daemons to connect...")
    daemons = server.list_daemons()
    while not daemons:
        time.sleep(1)
        daemons = server.list_daemons()

    daemon_id = daemons[0]
    print(f"✓ Found daemon: {daemon_id}\n")

    print("Starting interactive terminal...")
    print()

    # Start session in interactive mode - this blocks until user exits
    server.new_session(daemon_id, interactive=True)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
        sys.exit(0)
    except Exception as e:
        print(f"\n❌ Error: {e}")
        sys.exit(1)
