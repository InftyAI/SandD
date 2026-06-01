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
            result = server.execute_command(
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
            return server.execute_command(
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
        test_daemons = server.list_daemons(label_key="env", label_value="test")
        assert "daemon-debian-1" in test_daemons
        assert "daemon-debian-2" in test_daemons
        assert "daemon-alpine-1" in test_daemons
        assert "daemon-rocky-2" in test_daemons

        prod_daemons = server.list_daemons(label_key="env", label_value="prod")
        assert "daemon-alpine-2" in prod_daemons
        assert "daemon-rocky-1" in prod_daemons

    def test_filter_by_distro_label(self, server):
        """Filter daemons by distribution"""
        debian_daemons = server.list_daemons(label_key="distro", label_value="debian")
        assert "daemon-debian-1" in debian_daemons
        assert "daemon-debian-2" in debian_daemons
        assert len(debian_daemons) >= 2

        alpine_daemons = server.list_daemons(label_key="distro", label_value="alpine")
        assert "daemon-alpine-1" in alpine_daemons
        assert "daemon-alpine-2" in alpine_daemons

        rocky_daemons = server.list_daemons(label_key="distro", label_value="rocky")
        assert "daemon-rocky-1" in rocky_daemons
        assert "daemon-rocky-2" in rocky_daemons


class TestE2EResilience:
    """Test system resilience"""

    def test_daemon_restart(self, server):
        """Test daemon reconnection after container restart"""
        # Execute command before restart
        result = server.execute_command("daemon-debian-1", "echo 'before'", timeout=5)
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
        result = server.execute_command("daemon-debian-1", "echo 'after'", timeout=5)
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
        result = server.execute_command(
            "daemon-debian-1",
            "apt-get update && apt-get install -y htop",
            timeout=60
        )
        assert result.success

        result = server.execute_command("daemon-debian-1", "htop --version", timeout=5)
        assert result.success
        assert "htop" in result.stdout.lower()

    def test_package_manager_alpine(self, server):
        """Test apk package manager on Alpine daemons"""
        result = server.execute_command(
            "daemon-alpine-1",
            "apk update && apk add htop",
            timeout=60
        )
        assert result.success

        result = server.execute_command("daemon-alpine-1", "htop --version", timeout=5)
        assert result.success
        assert "htop" in result.stdout.lower()

    def test_package_manager_rocky(self, server):
        """Test dnf package manager on Rocky daemons"""
        result = server.execute_command(
            "daemon-rocky-1",
            "microdnf install -y htop",
            timeout=60
        )
        assert result.success

        result = server.execute_command("daemon-rocky-1", "htop --version", timeout=5)
        assert result.success
        assert "htop" in result.stdout.lower()

    def test_all_distros_run_same_command(self, server):
        """Verify all distributions can run common commands"""
        daemon_ids = [
            "daemon-debian-1",
            "daemon-alpine-1",
            "daemon-rocky-1"
        ]
        for daemon_id in daemon_ids:
            result = server.execute_command(daemon_id, "uname -s", timeout=5)
            assert result.success
            assert result.stdout.strip() == "Linux"

if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
