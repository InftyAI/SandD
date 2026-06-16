"""Integration tests for SandD with real daemon connections

These tests require the sandd binary to be built:
    make daemon-release

Run with: pytest python/tests/test_integration.py -v -s
"""
import pytest
import subprocess
import time
import os
import signal
from pathlib import Path
from sandd import Server


# Path to the daemon binary
DAEMON_BINARY = Path(__file__).parent.parent.parent / "target" / "release" / "sandd"


@pytest.fixture
def sandd_binary():
    """Check if sandd binary exists"""
    if not DAEMON_BINARY.exists():
        pytest.skip(
            f"Daemon binary not found at {DAEMON_BINARY}. "
            "Build it with: make daemon-release"
        )
    return str(DAEMON_BINARY)


@pytest.fixture
def server():
    """Create a server instance on a unique port"""
    # Use a different port for each test to avoid conflicts
    import random
    port = random.randint(9000, 9999)
    server = Server(host="127.0.0.1", port=port)
    yield server
    # Cleanup is automatic when server object is destroyed


@pytest.fixture
def daemon_process(server, sandd_binary):
    """Start a daemon process and connect it to the server"""
    daemon_id = f"test-daemon-{os.getpid()}"
    server_url = f"ws://127.0.0.1:{server.address.split(':')[1]}/ws"

    # Start daemon process
    proc = subprocess.Popen(
        [sandd_binary, "--server-url", server_url, "--daemon-id", daemon_id],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    # Wait for daemon to connect
    connected = server.wait_for_daemon(daemon_id, timeout=5.0)
    if not connected:
        proc.kill()
        pytest.fail(f"Daemon {daemon_id} failed to connect within timeout")

    yield daemon_id, proc

    # Cleanup: kill the daemon process
    try:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


class TestDaemonConnection:
    """Test daemon connection and lifecycle"""

    def test_daemon_connects(self, server, daemon_process):
        """Test that daemon successfully connects to server"""
        daemon_id, proc = daemon_process

        # Verify daemon is in the list
        daemons = server.list_daemons()
        daemon_ids = [d.id for d in daemons]
        assert daemon_id in daemon_ids

        # Verify daemon count
        assert server.daemon_count() == 1

    def test_multiple_daemons_connect(self, server, sandd_binary):
        """Test multiple daemons can connect simultaneously"""
        daemon_procs = []
        daemon_ids = []

        try:
            # Start 3 daemons
            for i in range(3):
                daemon_id = f"test-multi-daemon-{os.getpid()}-{i}"
                daemon_ids.append(daemon_id)
                server_url = f"ws://127.0.0.1:{server.address.split(':')[1]}/ws"

                proc = subprocess.Popen(
                    [sandd_binary, "--server-url", server_url, "--daemon-id", daemon_id],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                daemon_procs.append(proc)

            # Wait for all to connect
            for daemon_id in daemon_ids:
                assert server.wait_for_daemon(daemon_id, timeout=5.0)

            # Verify all connected
            daemons = server.list_daemons()
            daemon_id_list = [d.id for d in daemons]
            assert server.daemon_count() == 3
            for daemon_id in daemon_ids:
                assert daemon_id in daemon_id_list

        finally:
            # Cleanup all daemons
            for proc in daemon_procs:
                try:
                    proc.send_signal(signal.SIGTERM)
                    proc.wait(timeout=2)
                except:  # noqa: E722
                    proc.kill()

    def test_daemon_with_labels(self, server, sandd_binary):
        """Test daemon connection with labels and label-based filtering"""
        # Start daemon with env=prod and region=us-west labels
        daemon_id_prod = f"test-prod-daemon-{os.getpid()}"
        server_url = f"ws://127.0.0.1:{server.address.split(':')[1]}/ws"

        proc_prod = subprocess.Popen(
            [
                sandd_binary,
                "--server-url", server_url,
                "--daemon-id", daemon_id_prod,
                "--label", "env=prod",
                "--label", "region=us-west",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        # Start daemon with env=dev label
        daemon_id_dev = f"test-dev-daemon-{os.getpid()}"
        proc_dev = subprocess.Popen(
            [
                sandd_binary,
                "--server-url", server_url,
                "--daemon-id", daemon_id_dev,
                "--label", "env=dev",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            # Wait for both to connect
            assert server.wait_for_daemon(daemon_id_prod, timeout=5.0)
            assert server.wait_for_daemon(daemon_id_dev, timeout=5.0)

            # Test: list all daemons (no filter)
            all_daemons = server.list_daemons()
            all_ids = [d.id for d in all_daemons]
            assert daemon_id_prod in all_ids
            assert daemon_id_dev in all_ids
            assert len(all_daemons) >= 2

            # Test: filter by env=prod
            prod_daemons = server.list_daemons(labels={"env": "prod"})
            prod_ids = [d.id for d in prod_daemons]
            assert daemon_id_prod in prod_ids
            assert daemon_id_dev not in prod_ids

            # Test: filter by env=dev
            dev_daemons = server.list_daemons(labels={"env": "dev"})
            dev_ids = [d.id for d in dev_daemons]
            assert daemon_id_dev in dev_ids
            assert daemon_id_prod not in dev_ids

            # Test: filter by region=us-west
            region_daemons = server.list_daemons(labels={"region": "us-west"})
            region_ids = [d.id for d in region_daemons]
            assert daemon_id_prod in region_ids
            assert daemon_id_dev not in region_ids

            # Test: filter by multiple labels (AND logic)
            west_prod = server.list_daemons(labels={"env": "prod", "region": "us-west"})
            west_prod_ids = [d.id for d in west_prod]
            assert daemon_id_prod in west_prod_ids
            assert daemon_id_dev not in west_prod_ids

            # Test: filter by non-existent label
            none_daemons = server.list_daemons(labels={"env": "staging"})
            assert daemon_id_prod not in none_daemons
            assert daemon_id_dev not in none_daemons

        finally:
            proc_prod.kill()
            proc_dev.kill()


class TestCommandExecution:
    """Test command execution with real daemons"""

    def test_execute_simple_command(self, server, daemon_process):
        """Test executing a simple command"""
        daemon_id, _ = daemon_process

        result = server.exec(daemon_id, "echo 'Hello World'", timeout=5)

        assert result.success
        assert result.exit_code == 0
        assert "Hello World" in result.stdout
        assert result.duration_ms > 0

    def test_exec_with_failure(self, server, daemon_process):
        """Test executing a command that fails"""
        daemon_id, _ = daemon_process

        result = server.exec(daemon_id, "exit 42", timeout=5)

        assert not result.success
        assert result.exit_code == 42

    def test_exec_with_env(self, server, daemon_process):
        """Test executing command with environment variables"""
        daemon_id, _ = daemon_process

        env = {"TEST_VAR": "test_value_123"}
        if os.name == 'nt':  # Windows
            cmd = "echo %TEST_VAR%"
        else:  # Unix
            cmd = "echo $TEST_VAR"

        result = server.exec(daemon_id, cmd, timeout=5, env=env)

        assert result.success
        assert "test_value_123" in result.stdout

    def test_exec_with_cwd(self, server, daemon_process):
        """Test executing command with custom working directory"""
        daemon_id, _ = daemon_process

        result = server.exec(daemon_id, "pwd", timeout=5, cwd="/tmp")

        assert result.success
        # On some systems /tmp might be a symlink, so check both
        assert "/tmp" in result.stdout or "/private/tmp" in result.stdout

    def test_execute_long_output(self, server, daemon_process):
        """Test command with large output"""
        daemon_id, _ = daemon_process

        # Generate 1000 lines of output
        cmd = "for i in {1..1000}; do echo 'Line $i'; done"
        result = server.exec(daemon_id, cmd, timeout=10)

        assert result.success
        assert result.stdout.count('\n') >= 1000

    def test_command_timeout(self, server, daemon_process):
        """Test command timeout handling"""
        daemon_id, _ = daemon_process

        # Command that sleeps longer than timeout
        with pytest.raises(Exception):  # Should raise timeout or runtime error
            server.exec(daemon_id, "sleep 10", timeout=1)

    def test_execute_python_script(self, server, daemon_process):
        """Test executing Python code"""
        daemon_id, _ = daemon_process

        cmd = "python3 -c 'import sys; print(f\"Python {sys.version_info.major}.{sys.version_info.minor}\")'"
        result = server.exec(daemon_id, cmd, timeout=5)

        assert result.success
        assert "Python" in result.stdout


class TestGetDaemon:
    """Test get_daemon functionality with real daemons"""

    def test_get_existing_daemon(self, server, daemon_process):
        """Test get_daemon returns DaemonInfo for connected daemon"""
        daemon_id, _ = daemon_process

        daemon = server.get_daemon(daemon_id)

        assert daemon is not None
        assert daemon.id == daemon_id
        assert isinstance(daemon.version, str)
        assert isinstance(daemon.labels, dict)
        assert isinstance(daemon.is_busy, bool)

    def test_get_nonexistent_daemon(self, server, daemon_process):
        """Test get_daemon returns None for non-existent daemon"""
        _, _ = daemon_process

        result = server.get_daemon("definitely-not-a-real-daemon-id-12345")
        assert result is None

    def test_get_daemon_with_labels(self, server, sandd_binary):
        """Test get_daemon returns daemon with labels"""
        daemon_id = f"test-labeled-daemon-{os.getpid()}"
        server_url = f"ws://127.0.0.1:{server.address.split(':')[1]}/ws"

        proc = subprocess.Popen(
            [
                sandd_binary,
                "--server-url", server_url,
                "--daemon-id", daemon_id,
                "--label", "env=staging",
                "--label", "team=backend",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            assert server.wait_for_daemon(daemon_id, timeout=5.0)

            daemon = server.get_daemon(daemon_id)
            assert daemon is not None
            assert daemon.id == daemon_id
            assert daemon.labels == {"env": "staging", "team": "backend"}

        finally:
            proc.kill()

    def test_get_daemon_after_disconnect(self, server, sandd_binary):
        """Test get_daemon returns None after daemon disconnects"""
        daemon_id = f"test-disconnect-daemon-{os.getpid()}"
        server_url = f"ws://127.0.0.1:{server.address.split(':')[1]}/ws"

        proc = subprocess.Popen(
            [sandd_binary, "--server-url", server_url, "--daemon-id", daemon_id],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            # Wait for connection
            assert server.wait_for_daemon(daemon_id, timeout=5.0)

            # Verify daemon is there
            daemon = server.get_daemon(daemon_id)
            assert daemon is not None
            assert daemon.id == daemon_id

            # Kill the daemon
            proc.kill()
            proc.wait()

            # Give some time for disconnect to register
            time.sleep(0.5)

            # Daemon should no longer be found
            daemon = server.get_daemon(daemon_id)
            assert daemon is None
        finally:
            try:
                proc.kill()
            except:  # noqa: E722
                pass

    def test_get_daemon_multiple_times(self, server, daemon_process):
        """Test calling get_daemon multiple times returns consistent results"""
        daemon_id, _ = daemon_process

        # Call multiple times
        daemon1 = server.get_daemon(daemon_id)
        daemon2 = server.get_daemon(daemon_id)
        daemon3 = server.get_daemon(daemon_id)

        assert daemon1 is not None
        assert daemon2 is not None
        assert daemon3 is not None

        # All should have the same ID
        assert daemon1.id == daemon2.id == daemon3.id == daemon_id

    def test_get_daemon_busy_state(self, server, daemon_process):
        """Test get_daemon reflects busy state"""
        daemon_id, _ = daemon_process

        # Check initial state (should not be busy)
        daemon = server.get_daemon(daemon_id)
        assert daemon is not None
        assert daemon.is_busy is False

        # Start a long-running command in background
        import threading

        def run_long_command():
            try:
                server.exec(daemon_id, "sleep 2", timeout=5)
            except:  # noqa: E722
                pass

        thread = threading.Thread(target=run_long_command)
        thread.start()

        # Give command time to start
        time.sleep(0.2)

        # Check if daemon is now busy (might be, depending on timing)
        daemon_during = server.get_daemon(daemon_id)
        assert daemon_during is not None
        assert daemon_during.is_busy is True

        # Wait for command to complete
        thread.join()

        # Daemon should not be busy anymore
        daemon_after = server.get_daemon(daemon_id)
        assert daemon_after is not None
        assert daemon_after.is_busy is False

class TestServerStats:
    """Test server statistics with real connections"""

    def test_stats_with_connected_daemon(self, server, daemon_process):
        """Test server stats reflect connected daemon"""
        daemon_id, _ = daemon_process

        stats = server.get_stats()

        assert stats.total_daemons == 1
        assert isinstance(stats.by_platform, dict)
        assert len(stats.by_platform) > 0
        assert stats.oldest_connection_secs >= 0

    def test_stats_platform_reporting(self, server, daemon_process):
        """Test that stats report correct platform"""
        daemon_id, _ = daemon_process

        stats = server.get_stats()

        # Should have at least one platform
        platforms = list(stats.by_platform.keys())
        assert len(platforms) > 0

        # Common platform names (Rust's std::env::consts::OS values)
        assert any(p in ["linux", "macos", "windows", "Linux", "Darwin", "Windows"]
                  for p in platforms)


# class TestFileTransfer:
#     """Test file upload/download with real daemons"""

#     def test_upload_and_download_file(self, server, daemon_process):
#         """Test uploading and downloading a file"""
#         daemon_id, _ = daemon_process

#         # Create test data
#         test_data = b"Hello from SandD test!\nLine 2\nLine 3"
#         remote_path = f"/tmp/sandd-test-{os.getpid()}.txt"

#         try:
#             # Upload file
#             server.upload_file(daemon_id, remote_path, test_data)

#             # Verify file exists
#             result = server.exec(daemon_id, f"cat {remote_path}", timeout=5)
#             assert result.success
#             assert test_data.decode() in result.stdout

#             # Download file
#             downloaded_data = server.download_file(daemon_id, remote_path)
#             assert downloaded_data == test_data

#         finally:
#             # Cleanup
#             server.exec(daemon_id, f"rm -f {remote_path}", timeout=5)

#     def test_upload_large_file(self, server, daemon_process):
#         """Test uploading a larger file (1MB)"""
#         daemon_id, _ = daemon_process

#         # Create 1MB of test data
#         test_data = b"x" * (1024 * 1024)
#         remote_path = f"/tmp/sandd-large-test-{os.getpid()}.bin"

#         try:
#             server.upload_file(daemon_id, remote_path, test_data)

#             # Verify size
#             result = server.exec(
#                 daemon_id,
#                 f"wc -c < {remote_path}",
#                 timeout=5
#             )
#             assert result.success
#             size = int(result.stdout.strip())
#             assert size == len(test_data)

#         finally:
#             server.exec(daemon_id, f"rm -f {remote_path}", timeout=5)


class TestWaitForDaemon:
    """Test wait_for_daemon functionality"""

    def test_wait_for_existing_daemon(self, server, daemon_process):
        """Test waiting for already connected daemon"""
        daemon_id, _ = daemon_process

        # Should return immediately since daemon is already connected
        result = server.wait_for_daemon(daemon_id, timeout=1.0)
        assert result is True

    def test_wait_for_new_daemon(self, server, sandd_binary):
        """Test waiting for daemon that connects later"""
        daemon_id = f"test-delayed-daemon-{os.getpid()}"
        server_url = f"ws://127.0.0.1:{server.address.split(':')[1]}/ws"

        # Start waiting in one "thread" (we'll simulate with timing)
        import threading
        result_holder = {"connected": False}

        def wait_thread():
            result_holder["connected"] = server.wait_for_daemon(daemon_id, timeout=10.0)

        thread = threading.Thread(target=wait_thread)
        thread.start()

        # Wait a bit, then start the daemon
        time.sleep(0.5)
        proc = subprocess.Popen(
            [sandd_binary, "--server-url", server_url, "--daemon-id", daemon_id],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        try:
            thread.join(timeout=12.0)
            assert result_holder["connected"] is True

        finally:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()


@pytest.mark.skipif(
    not DAEMON_BINARY.exists(),
    reason="Requires compiled daemon binary"
)
class TestSession:
    """Test interactive sessions"""

    def test_session(self, server, daemon_process):
        """Test starting an interactive session"""
        daemon_id, _ = daemon_process

        session = server.new_session(daemon_id)
        assert session is not None

        # Write a command
        session.write(b"echo 'test123'\n")

        # Read output with timeout
        output = session.read(timeout=2.0)

        # Should contain our echo
        if output:
            output_str = output.decode('utf-8', errors='ignore')
            assert 'test123' in output_str or 'echo' in output_str

        # Close session
        session.close()


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
