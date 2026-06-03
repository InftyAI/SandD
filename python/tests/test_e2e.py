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
    compose_file = "docker-compose.e2e.yml"

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
        expected = [
            "daemon-debian-1", "daemon-debian-2",
            "daemon-alpine-1", "daemon-alpine-2",
            "daemon-rocky-1", "daemon-rocky-2"
        ]
        for daemon_id in expected:
            assert daemon_id in daemons
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


class TestE2ELabels:
    """Test label-based filtering in E2E"""

    def test_filter_by_env_label(self, server):
        """Filter daemons by env label"""
        test_daemons = server.list_daemons(labels={"env": "test"})
        assert "daemon-debian-1" in test_daemons
        assert "daemon-debian-2" in test_daemons
        assert "daemon-alpine-1" in test_daemons
        assert "daemon-rocky-2" in test_daemons

        prod_daemons = server.list_daemons(labels={"env": "prod"})
        assert "daemon-alpine-2" in prod_daemons
        assert "daemon-rocky-1" in prod_daemons

    def test_filter_by_distro_label(self, server):
        """Filter daemons by distribution"""
        debian_daemons = server.list_daemons(labels={"distro": "debian"})
        assert "daemon-debian-1" in debian_daemons
        assert "daemon-debian-2" in debian_daemons
        assert len(debian_daemons) >= 2

        alpine_daemons = server.list_daemons(labels={"distro": "alpine"})
        assert "daemon-alpine-1" in alpine_daemons
        assert "daemon-alpine-2" in alpine_daemons

        rocky_daemons = server.list_daemons(labels={"distro": "rocky"})
        assert "daemon-rocky-1" in rocky_daemons
        assert "daemon-rocky-2" in rocky_daemons


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
            session.write(b"for i in 1 2 3; do\n")
            time.sleep(0.2)
            session.write(b"echo $i\n")
            time.sleep(0.2)
            session.write(b"done\n")
            time.sleep(0.5)

            output = session.read(timeout=2.0)
            assert output is not None
            output_str = output.decode('utf-8', errors='ignore')
            # Should see the numbers
            assert '1' in output_str and '2' in output_str

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
