#!/usr/bin/env python3
"""
Example: Python agent using SandD (Sandbox Daemon)

This demonstrates how to use the server from a Python application
to execute commands on remote daemons.
"""

import time
from sandd import Server


def main():
    # Start server
    print("Starting sandbox server on 0.0.0.0:8765...")
    server = Server(host="0.0.0.0", port=8765)

    print(f"Server started: {server.address}")
    print("Waiting for daemons to connect...")
    print("(Start a daemon with: ./target/release/sandd --server-url ws://localhost:8765/ws)")
    print()

    # Wait for at least one daemon
    while server.daemon_count() == 0:
        time.sleep(1)
        print(".", end="", flush=True)

    print("\n")

    # List connected daemons
    daemons = server.list_daemons()
    print(f"✓ Connected daemons: {len(daemons)}")
    for daemon_id in daemons:
        print(f"  - {daemon_id}")
    print()

    # Get server stats
    stats = server.get_stats()
    print(f"Server stats:")
    print(f"  Total daemons: {stats.total_daemons}")
    print(f"  By platform: {stats.by_platform}")
    print(f"  Oldest connection: {stats.oldest_connection_secs}s")
    print()

    # Pick first daemon for demos
    daemon_id = daemons[0]
    print(f"Using daemon: {daemon_id}")
    print("=" * 60)
    print()

    # Example 1: Simple command execution
    print("Example 1: Execute simple command")
    print("-" * 60)
    result = server.execute_command(daemon_id, "echo 'Hello from daemon!'")
    print(f"Exit code: {result.exit_code}")
    print(f"Duration: {result.duration_ms}ms")
    print(f"Output: {result.stdout.strip()}")
    print()

    # Example 2: Command with environment variables
    print("Example 2: Command with environment")
    print("-" * 60)
    result = server.execute_command(
        daemon_id,
        "echo $MY_VAR",
        env={"MY_VAR": "custom_value"}
    )
    print(f"Output: {result.stdout.strip()}")
    print()

    # Example 3: List files
    print("Example 3: List files in /tmp")
    print("-" * 60)
    result = server.execute_command(daemon_id, "ls -lh /tmp | head -10")
    if result.success:
        print(result.stdout)
    else:
        print(f"Error: {result.stderr}")
    print()

    # Example 4: System information
    print("Example 4: Get system information")
    print("-" * 60)
    result = server.execute_command(daemon_id, "uname -a")
    print(f"System: {result.stdout.strip()}")

    result = server.execute_command(daemon_id, "uptime")
    print(f"Uptime: {result.stdout.strip()}")
    print()

    # Example 5: Interactive shell (basic demo)
    print("Example 5: Interactive shell session")
    print("-" * 60)
    try:
        shell = server.start_shell(daemon_id)
        print(f"Shell session started: {shell.session_id}")

        # Send commands
        shell.write(b"pwd\n")
        time.sleep(0.5)

        # Read output
        output = shell.read(timeout=1.0)
        if output:
            print(f"Output: {output.decode().strip()}")

        shell.write(b"echo 'Interactive shell works!'\n")
        time.sleep(0.5)

        output = shell.read(timeout=1.0)
        if output:
            print(f"Output: {output.decode().strip()}")

        print("✓ Shell session working")
        print()

    except Exception as e:
        print(f"Shell error: {e}")
        print()

    # Example 6: File operations (mock, since we need actual files)
    print("Example 6: File transfer")
    print("-" * 60)
    try:
        # Create test file
        test_data = b"Hello from agent!\nThis is a test file.\n"
        server.upload_file(daemon_id, "/tmp/test_upload.txt", test_data)
        print("✓ File uploaded to /tmp/test_upload.txt")

        # Verify with cat
        result = server.execute_command(daemon_id, "cat /tmp/test_upload.txt")
        print(f"File contents:\n{result.stdout}")

        # Download it back
        downloaded = server.download_file(daemon_id, "/tmp/test_upload.txt")
        print(f"✓ Downloaded {len(downloaded)} bytes")
        print(f"Match: {downloaded == test_data}")
        print()

    except Exception as e:
        print(f"File transfer error: {e}")
        print()

    # Example 7: Parallel execution (multiple commands)
    print("Example 7: Parallel command execution")
    print("-" * 60)
    import concurrent.futures

    commands = [
        "date",
        "whoami",
        "hostname",
        "pwd",
    ]

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
        futures = {
            executor.submit(server.execute_command, daemon_id, cmd): cmd
            for cmd in commands
        }

        for future in concurrent.futures.as_completed(futures):
            cmd = futures[future]
            result = future.result()
            print(f"{cmd:15} -> {result.stdout.strip()}")
    print()

    # Example 8: Error handling
    print("Example 8: Error handling")
    print("-" * 60)
    result = server.execute_command(daemon_id, "ls /nonexistent/directory")
    if not result.success:
        print(f"Command failed with exit code: {result.exit_code}")
        print(f"Error: {result.stderr.strip()}")
    print()

    print("=" * 60)
    print("All examples completed!")
    print(f"Final daemon count: {server.daemon_count()}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
    except Exception as e:
        print(f"\nError: {e}")
        import traceback
        traceback.print_exc()
