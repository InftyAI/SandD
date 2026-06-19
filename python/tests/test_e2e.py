"""End-to-end tests with Docker containers

Run with: make test-e2e
"""
import pytest
import time
import subprocess
from sandd import Server


@pytest.fixture(scope="module")
def docker_daemons():
    """Start Docker containers with daemons"""
    compose_file = "hack/docker/docker-compose.e2e.yml"

    # Build and start containers
    subprocess.run(
        ["docker", "compose", "-f", compose_file, "build"],
        check=True,
        capture_output=True
    )

    subprocess.run(
        ["docker", "compose", "-f", compose_file, "up", "-d"],
        check=True,
        capture_output=True
    )

    yield

    # Cleanup
    subprocess.run(
        ["docker", "compose", "-f", compose_file, "down"],
        capture_output=True
    )


@pytest.fixture(scope="module")
def server(docker_daemons):
    """Create server instance for E2E tests"""
    srv = Server(host="0.0.0.0", port=8765)

    # Wait for all daemons to connect (2 debian + 2 alpine + 2 rocky)
    daemon_ids = [
        "daemon-debian-1", "daemon-debian-2",
        "daemon-alpine-1", "daemon-alpine-2",
        "daemon-rocky-1", "daemon-rocky-2"
    ]
    for daemon_id in daemon_ids:
        connected = srv.wait_for_daemon(daemon_id, timeout=15.0)
        if not connected:
            pytest.fail(f"Daemon {daemon_id} failed to connect")

    yield srv


class TestE2EBasicOperations:
    """Basic E2E operations across Docker containers"""

    def test_all_daemons_connected(self, server):
        """Verify all 6 daemons connected (2 debian + 2 alpine + 2 rocky)"""
        daemons = server.list_daemons()
        daemon_ids = [d.id for d in daemons]
        expected = [
            "daemon-debian-1", "daemon-debian-2",
            "daemon-alpine-1", "daemon-alpine-2",
            "daemon-rocky-1", "daemon-rocky-2"
        ]
        for daemon_id in expected:
            assert daemon_id in daemon_ids
        assert server.daemon_count() >= 6

    def test_execute_on_each_daemon(self, server):
        """Execute commands on each daemon across all distributions"""
        daemon_ids = [
            "daemon-debian-1", "daemon-debian-2",
            "daemon-alpine-1", "daemon-alpine-2",
            "daemon-rocky-1", "daemon-rocky-2"
        ]
        for daemon_id in daemon_ids:
            result = server.exec(
                daemon_id,
                "echo 'Hello from container'",
                timeout=5
            )
            assert result.success
            assert "Hello from container" in result.stdout

    def test_concurrent_execution(self, server):
        """Execute commands concurrently on multiple daemons"""
        import concurrent.futures

        daemon_ids = [
            "daemon-debian-1", "daemon-alpine-1", "daemon-rocky-1"
        ]

        def run_cmd(daemon_id):
            return server.exec(
                daemon_id,
                f"echo 'Response from {daemon_id}'",
                timeout=5
            )

        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
            futures = [executor.submit(run_cmd, did) for did in daemon_ids]
            results = [f.result() for f in futures]

        assert all(r.success for r in results)
        assert all("Response from" in r.stdout for r in results)

    def test_concurrent_execution_same_daemon(self, server):
        """Execute multiple commands on the same daemon (processed sequentially)"""
        import concurrent.futures

        daemon_id = "daemon-debian-1"

        def run_sleep(n):
            result = server.exec(daemon_id, f"sleep {n} && echo 'slept {n}s'", timeout=10)
            return result

        def run_fast():
            result = server.exec(daemon_id, "echo 'fast command'", timeout=5)
            return result

        start = time.time()
        # Submit both commands - daemon processes them sequentially
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
            slow_future = executor.submit(run_sleep, 3)
            fast_future = executor.submit(run_fast)

            # Both commands succeed
            fast_result = fast_future.result()
            assert fast_result.success
            assert "fast command" in fast_result.stdout

            slow_result = slow_future.result()
            assert slow_result.success
            assert "slept 3s" in slow_result.stdout

            # Total time is ~3s (sequential: slow command blocks fast one)
            duration = time.time() - start
            assert 2.5 < duration < 4.0  # Sequential processing


class TestE2EBroadcast:
    """Test broadcast operations"""

    def test_broadcast_simple_command(self, server):
        """Broadcast a simple command to multiple daemons"""
        results = server.broadcast(
            labels={"env": "test"},
            command="echo 'hello from broadcast'"
        )

        # Should have 4 test daemons
        assert len(results) == 4

        # Check all succeeded
        for _, result in results.items():
            assert result.success
            assert "hello from broadcast" in result.stdout

    def test_broadcast_with_multiple_labels(self, server):
        """Broadcast with multiple label filters (AND logic)"""
        results = server.broadcast(
            labels={"env": "test", "distro": "debian"},
            command="hostname"
        )

        # Should match only debian test daemons
        assert len(results) == 2
        assert "daemon-debian-1" in results
        assert "daemon-debian-2" in results

        for result in results.values():
            assert result.success

    def test_broadcast_no_matching_daemons(self, server):
        """Broadcast with labels that match no daemons"""
        results = server.broadcast(
            labels={"env": "nonexistent"},
            command="hostname"
        )

        # Should return empty dict
        assert len(results) == 0

    def test_broadcast_with_failure(self, server):
        """Broadcast command that fails on some daemons"""
        results = server.broadcast(
            labels={"env": "prod"},
            command="exit 1"
        )

        # Should have results for prod daemons
        assert len(results) == 2

        # All should have exit code 1
        for result in results.values():
            assert not result.success
            assert result.exit_code == 1

    def test_broadcast_concurrent_execution(self, server):
        """Verify broadcast executes concurrently, not serially"""

        # Broadcast a 2-second sleep to test daemons
        start = time.time()
        results = server.broadcast(
            labels={"env": "test"},
            command="sleep 2"
        )
        duration = time.time() - start

        # Should complete in ~2-3 seconds (concurrent), not 8+ seconds (serial)
        assert len(results) == 4
        assert 2.0 < duration < 3.0

        for result in results.values():
            assert result.success


class TestE2ELabels:
    """Test label-based filtering in E2E"""

    def test_filter_by_env_label(self, server):
        """Filter daemons by env label"""
        test_daemons = server.list_daemons(labels={"env": "test"})
        test_ids = [d.id for d in test_daemons]
        assert "daemon-debian-1" in test_ids
        assert "daemon-debian-2" in test_ids
        assert "daemon-alpine-1" in test_ids
        assert "daemon-rocky-2" in test_ids

        prod_daemons = server.list_daemons(labels={"env": "prod"})
        prod_ids = [d.id for d in prod_daemons]
        assert "daemon-alpine-2" in prod_ids
        assert "daemon-rocky-1" in prod_ids

    def test_filter_by_distro_label(self, server):
        """Filter daemons by distribution"""
        debian_daemons = server.list_daemons(labels={"distro": "debian"})
        debian_ids = [d.id for d in debian_daemons]
        assert "daemon-debian-1" in debian_ids
        assert "daemon-debian-2" in debian_ids
        assert len(debian_daemons) >= 2

        alpine_daemons = server.list_daemons(labels={"distro": "alpine"})
        alpine_ids = [d.id for d in alpine_daemons]
        assert "daemon-alpine-1" in alpine_ids
        assert "daemon-alpine-2" in alpine_ids

        rocky_daemons = server.list_daemons(labels={"distro": "rocky"})
        rocky_ids = [d.id for d in rocky_daemons]
        assert "daemon-rocky-1" in rocky_ids
        assert "daemon-rocky-2" in rocky_ids


class TestE2EResilience:
    """Test system resilience"""

    def test_daemon_restart(self, server):
        """Test daemon reconnection after container restart"""
        # Execute command before restart
        result = server.exec("daemon-debian-1", "echo 'before'", timeout=5)
        assert result.success

        # Restart container
        subprocess.run(
            ["docker", "restart", "sandd-daemon-debian-1"],
            check=True,
            capture_output=True
        )

        # Wait for reconnection
        time.sleep(5)
        reconnected = server.wait_for_daemon("daemon-debian-1", timeout=15.0)
        assert reconnected

        # Execute command after restart
        result = server.exec("daemon-debian-1", "echo 'after'", timeout=5)
        assert result.success
        assert "after" in result.stdout


class TestE2EStats:
    """Test statistics with Docker daemons"""

    def test_stats_reflect_containers(self, server):
        """Verify stats show all container daemons"""
        stats = server.get_stats()
        assert stats.total_daemons >= 6
        assert "linux" in [p.lower() for p in stats.by_platform.keys()]


class TestE2EDistributionSpecific:
    """Test distribution-specific commands"""

    def test_package_manager_debian(self, server):
        """Test apt package manager on Debian daemons"""
        result = server.exec(
            "daemon-debian-1",
            "apt-get update && apt-get install -y curl",
            timeout=60
        )
        assert result.success

        result = server.exec("daemon-debian-1", "curl --version", timeout=5)
        assert result.success
        assert "curl" in result.stdout

    def test_package_manager_alpine(self, server):
        """Test apk package manager on Alpine daemons"""
        result = server.exec(
            "daemon-alpine-1",
            "apk update && apk add curl",
            timeout=60
        )
        assert result.success

        result = server.exec("daemon-alpine-1", "curl --version", timeout=5)
        assert result.success
        assert "curl" in result.stdout

    def test_package_manager_rocky(self, server):
        """Test dnf package manager on Rocky daemons"""
        result = server.exec(
            "daemon-rocky-1",
            "microdnf install -y curl",
            timeout=60
        )
        assert result.success

        result = server.exec("daemon-rocky-1", "curl --version", timeout=5)
        assert result.success
        assert "curl" in result.stdout

    def test_all_distros_run_same_command(self, server):
        """Verify all distributions can run common commands"""
        daemon_ids = [
            "daemon-debian-1",
            "daemon-alpine-1",
            "daemon-rocky-1"
        ]
        for daemon_id in daemon_ids:
            result = server.exec(daemon_id, "uname -s", timeout=5)
            assert result.success
            assert result.stdout.strip() == "Linux"


class TestE2ESessionSessions:
    """Test interactive sessions across distributions"""

    def test_session_basic_commands(self, server):
        """Test basic session interaction"""
        daemon_id = "daemon-debian-1"

        session = server.new_session(daemon_id)
        assert session is not None

        try:
            # Send a command
            session.write(b"echo 'Hello from session'\n")
            time.sleep(0.5)

            # Read output
            output = session.read(timeout=2.0)
            assert output is not None
            output_str = output.decode('utf-8', errors='ignore')
            assert 'Hello from session' in output_str

        finally:
            session.close()

    def test_session_across_distributions(self, server):
        """Test session works on all distributions"""
        daemon_ids = [
            "daemon-debian-1",
            "daemon-alpine-1",
            "daemon-rocky-1"
        ]

        for daemon_id in daemon_ids:
            session = server.new_session(daemon_id)
            assert session is not None

            try:
                # Test command execution
                session.write(b"whoami\n")
                time.sleep(0.3)

                output = session.read(timeout=2.0)
                assert output is not None
                # Should see some output (username)
                assert len(output) > 0

            finally:
                session.close()

    def test_session_multiline_commands(self, server):
        """Test multi-line commands in session"""
        daemon_id = "daemon-alpine-1"

        session = server.new_session(daemon_id)
        assert session is not None

        try:
            # Send multi-line command
            session.write(b"for i in 1 2 3; do echo $i; done\n")
            time.sleep(0.5)

            # Read all output chunks
            all_output = b''
            for _ in range(5):
                output = session.read(timeout=0.5)
                if output:
                    all_output += output
                else:
                    break

            assert all_output
            output_str = all_output.decode('utf-8', errors='ignore')
            # Should see the numbers
            assert '1' in output_str and '2' in output_str and '3' in output_str

        finally:
            session.close()

    def test_session_environment_variables(self, server):
        """Test setting and reading environment variables"""
        daemon_id = "daemon-rocky-1"

        session = server.new_session(daemon_id)
        assert session is not None

        try:
            # Set environment variable
            session.write(b"export TEST_VAR='test123'\n")
            time.sleep(0.3)

            # Read it back
            session.write(b"echo $TEST_VAR\n")
            time.sleep(0.3)

            output = session.read(timeout=2.0)
            assert output is not None
            output_str = output.decode('utf-8', errors='ignore')
            assert 'test123' in output_str

        finally:
            session.close()

    def test_session_cd_persistence(self, server):
        """Test that cd persists within a session session"""
        daemon_id = "daemon-debian-1"

        session = server.new_session(daemon_id)
        assert session is not None

        try:
            # Change directory
            session.write(b"cd /tmp\n")
            time.sleep(0.3)

            # Verify we're in /tmp
            session.write(b"pwd\n")
            time.sleep(0.3)

            output = session.read(timeout=2.0)
            assert output is not None
            output_str = output.decode('utf-8', errors='ignore')
            assert '/tmp' in output_str

        finally:
            session.close()


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
