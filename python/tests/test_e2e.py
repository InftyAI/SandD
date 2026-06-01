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

    # Wait for all daemons to connect
    for daemon_id in ["daemon-1", "daemon-2", "daemon-3"]:
        connected = srv.wait_for_daemon(daemon_id, timeout=10.0)
        if not connected:
            pytest.fail(f"Daemon {daemon_id} failed to connect")

    yield srv


class TestE2EBasicOperations:
    """Basic E2E operations across Docker containers"""

    def test_all_daemons_connected(self, server):
        """Verify all 3 daemons connected"""
        daemons = server.list_daemons()
        assert "daemon-1" in daemons
        assert "daemon-2" in daemons
        assert "daemon-3" in daemons
        assert server.daemon_count() == 3

    def test_execute_on_each_daemon(self, server):
        """Execute commands on each daemon"""
        for daemon_id in ["daemon-1", "daemon-2", "daemon-3"]:
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

        def run_cmd(daemon_id):
            return server.execute_command(
                daemon_id,
                f"echo 'Response from {daemon_id}'",
                timeout=5
            )

        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
            futures = [
                executor.submit(run_cmd, f"daemon-{i}")
                for i in range(1, 4)
            ]
            results = [f.result() for f in futures]

        assert all(r.success for r in results)
        assert all("Response from" in r.stdout for r in results)


class TestE2ELabels:
    """Test label-based filtering in E2E"""

    def test_filter_by_env_label(self, server):
        """Filter daemons by env label"""
        test_daemons = server.list_daemons(label_key="env", label_value="test")
        assert "daemon-1" in test_daemons
        assert "daemon-2" in test_daemons
        assert "daemon-3" not in test_daemons

        prod_daemons = server.list_daemons(label_key="env", label_value="prod")
        assert "daemon-3" in prod_daemons
        assert "daemon-1" not in prod_daemons

    def test_filter_by_region_label(self, server):
        """Filter daemons by region label"""
        us_east = server.list_daemons(label_key="region", label_value="us-east")
        assert "daemon-1" in us_east

        eu_west = server.list_daemons(label_key="region", label_value="eu-west")
        assert "daemon-3" in eu_west


class TestE2EResilience:
    """Test system resilience"""

    def test_daemon_restart(self, server):
        """Test daemon reconnection after container restart"""
        # Execute command before restart
        result = server.execute_command("daemon-1", "echo 'before'", timeout=5)
        assert result.success

        # Restart container
        subprocess.run(
            ["docker", "restart", "sandd-daemon-1"],
            check=True,
            capture_output=True
        )

        # Wait for reconnection
        time.sleep(5)
        reconnected = server.wait_for_daemon("daemon-1", timeout=15.0)
        assert reconnected

        # Execute command after restart
        result = server.execute_command("daemon-1", "echo 'after'", timeout=5)
        assert result.success
        assert "after" in result.stdout


class TestE2EStats:
    """Test statistics with Docker daemons"""

    def test_stats_reflect_containers(self, server):
        """Verify stats show all container daemons"""
        stats = server.get_stats()
        assert stats.total_daemons == 3
        assert "linux" in [p.lower() for p in stats.by_platform.keys()]

class TestE2ECommandExecution:
    """Test command execution across Docker daemons"""

    def test_command_output(self, server):
        """Verify command output from daemons"""
        for daemon_id in ["daemon-1", "daemon-2", "daemon-3"]:
            result = server.execute_command(
                daemon_id,
                "uname -s",
                timeout=5
            )
            assert result.success
            assert result.stdout.strip() == "Linux"


    def test_execute_install_command(self, server):
        """Test executing an installation command on daemons"""
        for daemon_id in ["daemon-1", "daemon-2", "daemon-3"]:
            result = server.execute_command(
                daemon_id,
                "apt-get update && apt-get install -y curl",
                timeout=30
            )

            assert result.success, f"Command failed on {daemon_id}: {result.stderr}"

        for daemon_id in ["daemon-1", "daemon-2", "daemon-3"]:
            result = server.execute_command(
                daemon_id,
                "curl --version",
                timeout=5
            )
            assert result.success, f"Command failed on {daemon_id}: {result.stderr}"
            assert "curl" in result.stdout

if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
