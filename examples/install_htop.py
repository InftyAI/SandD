#!/usr/bin/env python3
"""Example: Installing htop on daemons

This example shows how to check if htop is available and install it if needed.
htop is a small, interactive process viewer - perfect for demonstrating
package installation across different distributions.

Usage:
    python examples/install_htop.py
"""

from sandd import Server
import sys
import time


def check_htop_available(server, daemon_id):
    """Check if htop is available on a daemon"""
    result = server.exec(daemon_id, "which htop", timeout=5)
    return result.success


def install_htop(server, daemon_id):
    """Install htop on a daemon using the system package manager"""
    print(f"Installing htop on {daemon_id}...")

    # Detect platform (macOS vs Linux)
    platform_result = server.exec(daemon_id, "uname -s", timeout=5)
    if not platform_result.success:
        print("❌ Could not detect platform")
        return False

    platform = platform_result.stdout.strip()

    # Handle macOS
    if platform == "Darwin":
        print("  Detected macOS, using Homebrew...")
        cmd = "brew install htop"
    else:
        # Linux - detect distribution
        distro_result = server.exec(
            daemon_id,
            "cat /etc/os-release 2>/dev/null || echo 'unknown'",
            timeout=5
        )

        if not distro_result.success:
            print("❌ Could not detect distribution")
            return False

        distro = distro_result.stdout.lower()

        # Determine install command based on distribution
        if "alpine" in distro:
            cmd = "apk update && apk add htop"
        elif "ubuntu" in distro or "debian" in distro:
            cmd = "apt-get update && apt-get install -y htop"
        elif "rocky" in distro or "rhel" in distro or "centos" in distro:
            cmd = "microdnf install -y htop || dnf install -y htop || yum install -y htop"
        elif "fedora" in distro:
            cmd = "dnf install -y htop"
        else:
            print("❌ Unknown Linux distribution")
            return False

    # Execute installation
    result = server.exec(daemon_id, cmd, timeout=120)

    if result.success:
        print("✓ htop installed successfully")
        return True
    else:
        print(f"❌ Failed to install htop: {result.stderr}")
        return False


def main():
    print("htop Installation Example")
    print("=" * 50)

    # Connect to server
    server = Server("127.0.0.1", 8765)
    print(f"✓ Server started on {server.address}\n")

    # Wait for at least one daemon
    print("Waiting for daemons to connect...")
    print("(Start a daemon with: ./target/release/sandd --server-url ws://127.0.0.1:8765/ws)")
    daemons = server.list_daemons()
    while not daemons:
        time.sleep(1)
        daemons = server.list_daemons()

    daemon = daemons[0]
    daemon_id = daemon.id
    print(f"✓ Found daemon: {daemon_id} (version={daemon.version})\n")

    # Check if htop is available
    print("Checking if htop is available...")
    if check_htop_available(server, daemon_id):
        print("✓ htop is already installed")

        # Get htop version
        result = server.exec(daemon_id, "htop --version", timeout=5)
        if result.success:
            # htop version is usually first line
            version_line = result.stdout.split('\n')[0]
            print(f"  {version_line}")
    else:
        print("✗ htop is not installed")
        print()

        # Install htop
        if install_htop(server, daemon_id):
            # Verify installation
            result = server.exec(daemon_id, "htop --version", timeout=5)
            if result.success:
                version_line = result.stdout.split('\n')[0]
                print(f"  {version_line}")
        else:
            print("Failed to install htop")
            sys.exit(1)

    print()

    # Demonstrate htop is working by showing process info
    print("Testing htop functionality...")
    print("-" * 50)

    # Since htop is interactive, we can't run it directly
    # But we can verify it works by checking its help and showing system info
    test_commands = [
        ("htop --help | head -5", "Show htop help"),
        ("ps aux | head -10", "Show running processes (what htop displays)"),
        ("uptime", "Show system uptime"),
    ]

    for cmd, description in test_commands:
        result = server.exec(daemon_id, cmd, timeout=10)
        if result.success:
            print(f"✓ {description}")
            # Show first few lines of output
            output_lines = result.stdout.strip().split('\n')[:3]
            for line in output_lines:
                print(f"  {line}")
            if len(result.stdout.strip().split('\n')) > 3:
                print("  ...")
            print()
        else:
            print(f"✗ {description}: {result.stderr}\n")

    print("=" * 50)
    print("Example complete!")
    print()
    print("Note: htop is an interactive tool. To use it interactively,")
    print("      use server.session() instead of exec().")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
        sys.exit(0)
    except Exception as e:
        print(f"\n❌ Error: {e}")
        sys.exit(1)
