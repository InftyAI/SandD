#!/usr/bin/env python3
"""Example: Installing git on daemons

This example shows how to check if git is available and install it if needed.

Usage:
    python examples/install_git.py
"""

from sandd import Server
import sys
import time


def check_git_available(server, daemon_id):
    """Check if git is available on a daemon"""
    result = server.execute_command(daemon_id, "which git", timeout=5)
    return result.success


def install_git(server, daemon_id):
    """Install git on a daemon using the system package manager"""
    print(f"Installing git on {daemon_id}...")

    # Detect platform
    platform_result = server.execute_command(daemon_id, "uname -s", timeout=5)
    if not platform_result.success:
        print("❌ Could not detect platform")
        return False

    platform = platform_result.stdout.strip().lower()

    # Determine install command
    if "linux" in platform:
        # Check if it's Debian/Ubuntu or RHEL/CentOS
        distro_result = server.execute_command(
            daemon_id,
            "cat /etc/os-release 2>/dev/null || echo 'unknown'",
            timeout=5
        )
        distro = distro_result.stdout.lower()

        if "ubuntu" in distro or "debian" in distro:
            cmd = "sudo apt-get update && sudo apt-get install -y git"
        elif "centos" in distro or "rhel" in distro or "fedora" in distro:
            cmd = "sudo yum install -y git"
        else:
            # Default to apt for unknown Linux
            cmd = "sudo apt-get update && sudo apt-get install -y git"

    elif "darwin" in platform:
        cmd = "brew install git"
    else:
        print(f"❌ Unsupported platform: {platform}")
        return False

    # Execute installation
    result = server.execute_command(daemon_id, cmd, timeout=300)

    if result.success:
        print("✓ git installed successfully")
        return True
    else:
        print(f"❌ Failed to install git: {result.stderr}")
        return False


def main():
    print("Git Installation Example")
    print("=" * 50)

    # Connect to server
    server = Server("127.0.0.1", 8765)
    print(f"✓ Server started on {server.address}\n")

    # Wait for at least one daemon
    print("Waiting for daemons to connect...")
    daemons = server.list_daemons()
    while not daemons:
        time.sleep(1)
        daemons = server.list_daemons()

    daemon_id = daemons[0]
    print(f"✓ Found daemon: {daemon_id}\n")

    # Check if git is available
    print("Checking if git is available...")
    if check_git_available(server, daemon_id):
        print("✓ git is already installed")

        # Get git version
        result = server.execute_command(daemon_id, "git --version", timeout=5)
        print(f"  Version: {result.stdout.strip()}")
    else:
        print("✗ git is not installed")
        print()

        # Install git
        if install_git(server, daemon_id):
            # Verify installation
            result = server.execute_command(daemon_id, "git --version", timeout=5)
            if result.success:
                print(f"  Version: {result.stdout.strip()}")
        else:
            print("Failed to install git")
            sys.exit(1)

    print()

    # Test git functionality
    print("Testing git functionality...")
    print("-" * 50)

    # Create a test repo
    test_commands = [
        ("mkdir -p /tmp/test-repo && cd /tmp/test-repo", "Create test directory"),
        ("git init", "Initialize git repo"),
        ("git config user.name 'Test User'", "Configure user"),
        ("git config user.email 'test@example.com'", "Configure email"),
        ("echo 'Hello from SandD' > README.md", "Create file"),
        ("git add README.md", "Stage file"),
        ("git commit -m 'Initial commit'", "Create commit"),
        ("git log --oneline", "Show commit"),
    ]

    for cmd, description in test_commands:
        result = server.execute_command(daemon_id, cmd, timeout=10)
        if result.success:
            print(f"✓ {description}")
            if "git log" in cmd:
                print(f"  {result.stdout.strip()}")
        else:
            print(f"✗ {description}: {result.stderr}")

    print()
    print("=" * 50)
    print("Example complete!")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
        sys.exit(0)
    except Exception as e:
        print(f"\n❌ Error: {e}")
        sys.exit(1)
