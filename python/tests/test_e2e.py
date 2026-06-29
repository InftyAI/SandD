"""End-to-end tests with Docker containers

Run with: make test-e2e

These tests are marked as 'e2e' and skipped by default in 'make test'.
Use 'make test-e2e' to run them explicitly.
"""
import pytest
import time
import subprocess
from sandd import Server

pytestmark = pytest.mark.e2e


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


class TestE2ESnapshots:
    """E2E snapshot operations"""

    def test_create_and_list_snapshot(self, server):
        """Create snapshot and list it"""
        daemon_id = "daemon-debian-1"

        # Create a test workspace
        server.exec(daemon_id, "mkdir -p /tmp/test-workspace", timeout=5)
        server.exec(daemon_id, "echo 'test content' > /tmp/test-workspace/file.txt", timeout=5)

        # Create snapshot
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/test-workspace",
            message="Test snapshot",
            tags=["test"]
        )
        assert snapshot_id is not None
        assert len(snapshot_id) > 0

        # List snapshots
        snapshots = server.list_snapshots(daemon_id)
        assert len(snapshots) > 0
        assert any(s.id == snapshot_id for s in snapshots)

        # Find by tag
        found = server.list_snapshots(daemon_id, tags=["test"])
        assert len(found) > 0
        assert found[0].id == snapshot_id
        assert found[0].message == "Test snapshot"
        assert "test" in found[0].tags

    def test_create_and_restore_snapshot(self, server):
        """Create snapshot and restore it"""
        daemon_id = "daemon-alpine-1"

        # Create test workspace
        server.exec(daemon_id, "mkdir -p /tmp/source", timeout=5)
        server.exec(daemon_id, "echo 'original' > /tmp/source/data.txt", timeout=5)

        # Create snapshot
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/source",
            message="Original state"
        )

        # Verify snapshot created
        snapshots = server.list_snapshots(daemon_id)
        assert any(s.id == snapshot_id for s in snapshots)

        # Restore to different location
        file_count = server.restore_snapshot(
            daemon_id,
            snapshot_id,
            "/tmp/restored"
        )
        assert file_count > 0

        # Verify restored content
        result = server.exec(daemon_id, "cat /tmp/restored/data.txt", timeout=5)
        assert result.success
        assert "original" in result.stdout

    def test_snapshot_with_multiple_tags(self, server):
        """Create snapshot with multiple tags"""
        daemon_id = "daemon-rocky-1"

        # Create workspace
        server.exec(daemon_id, "mkdir -p /tmp/multi-tag", timeout=5)
        server.exec(daemon_id, "echo 'tagged' > /tmp/multi-tag/file.txt", timeout=5)

        # Create snapshot with multiple tags
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/multi-tag",
            message="Multi-tagged",
            tags=["v1.0.0", "stable", "production"]
        )

        # List by different tags
        by_v1 = server.list_snapshots(daemon_id, tags=["v1.0.0"])
        by_stable = server.list_snapshots(daemon_id, tags=["stable"])
        by_production = server.list_snapshots(daemon_id, tags=["production"])

        assert len(by_v1) > 0 and by_v1[0].id == snapshot_id
        assert len(by_stable) > 0 and by_stable[0].id == snapshot_id
        assert len(by_production) > 0 and by_production[0].id == snapshot_id

    def test_snapshot_immutable_tags(self, server):
        """Verify tags are immutable (duplicate tag should fail)"""
        daemon_id = "daemon-debian-2"

        # Create workspace
        server.exec(daemon_id, "mkdir -p /tmp/immutable-tag", timeout=5)
        server.exec(daemon_id, "echo 'first' > /tmp/immutable-tag/data.txt", timeout=5)

        # Create first snapshot with tag
        snapshot_id1 = server.create_snapshot(
            daemon_id,
            "/tmp/immutable-tag",
            tags=["unique-tag"]
        )
        assert snapshot_id1 is not None

        # Try to create second snapshot with same tag (should fail)
        server.exec(daemon_id, "echo 'second' > /tmp/immutable-tag/data.txt", timeout=5)

        with pytest.raises(Exception) as exc_info:
            server.create_snapshot(
                daemon_id,
                "/tmp/immutable-tag",
                tags=["unique-tag"]
            )
        assert "already exists" in str(exc_info.value).lower()

    def test_delete_snapshot(self, server):
        """Delete snapshot and verify it's removed"""
        daemon_id = "daemon-alpine-2"

        # Create workspace
        server.exec(daemon_id, "mkdir -p /tmp/delete-test", timeout=5)
        server.exec(daemon_id, "echo 'to delete' > /tmp/delete-test/file.txt", timeout=5)

        # Create snapshot with tag
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/delete-test",
            message="Will be deleted",
            tags=["delete-me"]
        )

        # Verify snapshot exists
        snapshots_before = server.list_snapshots(daemon_id)
        assert any(s.id == snapshot_id for s in snapshots_before)

        # Delete snapshot
        server.delete_snapshot(daemon_id, snapshot_id)

        # Verify snapshot removed
        snapshots_after = server.list_snapshots(daemon_id)
        assert not any(s.id == snapshot_id for s in snapshots_after)

        # Verify tag can be reused after deletion
        snapshot_id2 = server.create_snapshot(
            daemon_id,
            "/tmp/delete-test",
            tags=["delete-me"]  # Should work now
        )
        assert snapshot_id2 is not None
        assert snapshot_id2 != snapshot_id

    def test_find_snapshot_by_tag(self, server):
        """Find snapshot by tag (O(1) lookup)"""
        daemon_id = "daemon-rocky-2"

        # Create workspace
        server.exec(daemon_id, "mkdir -p /tmp/find-test", timeout=5)
        server.exec(daemon_id, "echo 'findme' > /tmp/find-test/data.txt", timeout=5)

        # Create snapshot with unique tag
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/find-test",
            message="Find me by tag",
            tags=["unique-find-tag"]
        )

        # Find by tag
        found = server.find_snapshot_by_tag(daemon_id, "unique-find-tag")
        assert found is not None
        assert found.id == snapshot_id
        assert found.message == "Find me by tag"
        assert "unique-find-tag" in found.tags

        # Try to find non-existent tag
        not_found = server.find_snapshot_by_tag(daemon_id, "non-existent-tag")
        assert not_found is None

    def test_get_snapshot(self, server):
        """Get snapshot details by ID"""
        daemon_id = "daemon-debian-1"

        # Create workspace
        server.exec(daemon_id, "mkdir -p /tmp/get-test", timeout=5)
        server.exec(daemon_id, "echo 'data1' > /tmp/get-test/file1.txt", timeout=5)
        server.exec(daemon_id, "echo 'data2' > /tmp/get-test/file2.txt", timeout=5)

        # Create snapshot
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/get-test",
            message="Get test snapshot",
            tags=["get-tag-1", "get-tag-2"]
        )

        # Get snapshot details
        snapshot = server.get_snapshot(daemon_id, snapshot_id)
        assert snapshot.id == snapshot_id
        assert snapshot.message == "Get test snapshot"
        assert snapshot.tags == ["get-tag-1", "get-tag-2"]
        assert snapshot.file_count == 2
        assert snapshot.total_size > 0

        # Try to get non-existent snapshot
        with pytest.raises(Exception):
            server.get_snapshot(daemon_id, "non-existent-id")

    def test_snapshot_nested_directories(self, server):
        """Verify nested directory structure is preserved"""
        daemon_id = "daemon-debian-2"

        # Create nested directory structure
        server.exec(daemon_id, "mkdir -p /tmp/nested/a/b/c", timeout=5)
        server.exec(daemon_id, "echo 'file1' > /tmp/nested/file1.txt", timeout=5)
        server.exec(daemon_id, "echo 'file2' > /tmp/nested/a/file2.txt", timeout=5)
        server.exec(daemon_id, "echo 'file3' > /tmp/nested/a/b/file3.txt", timeout=5)
        server.exec(daemon_id, "echo 'file4' > /tmp/nested/a/b/c/file4.txt", timeout=5)

        # Create snapshot
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/nested",
            message="Nested structure"
        )

        # Restore
        server.restore_snapshot(daemon_id, snapshot_id, "/tmp/restored-nested")

        # Verify all files and structure
        result1 = server.exec(daemon_id, "cat /tmp/restored-nested/file1.txt", timeout=5)
        assert result1.success and "file1" in result1.stdout

        result2 = server.exec(daemon_id, "cat /tmp/restored-nested/a/file2.txt", timeout=5)
        assert result2.success and "file2" in result2.stdout

        result3 = server.exec(daemon_id, "cat /tmp/restored-nested/a/b/file3.txt", timeout=5)
        assert result3.success and "file3" in result3.stdout

        result4 = server.exec(daemon_id, "cat /tmp/restored-nested/a/b/c/file4.txt", timeout=5)
        assert result4.success and "file4" in result4.stdout

    def test_snapshot_binary_files(self, server):
        """Verify binary files are correctly captured and restored"""
        daemon_id = "daemon-alpine-1"

        # Create workspace with binary file
        server.exec(daemon_id, "mkdir -p /tmp/binary-test", timeout=5)
        # Create a small binary file
        server.exec(daemon_id, "dd if=/dev/urandom of=/tmp/binary-test/random.bin bs=1024 count=10", timeout=5)

        # Get checksum before snapshot
        result_before = server.exec(daemon_id, "md5sum /tmp/binary-test/random.bin", timeout=5)
        assert result_before.success
        checksum_before = result_before.stdout.split()[0]

        # Create snapshot
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/binary-test",
            message="Binary file test"
        )

        # Restore
        server.restore_snapshot(daemon_id, snapshot_id, "/tmp/restored-binary")

        # Verify checksum matches
        result_after = server.exec(daemon_id, "md5sum /tmp/restored-binary/random.bin", timeout=5)
        assert result_after.success
        checksum_after = result_after.stdout.split()[0]

        assert checksum_before == checksum_after, "Binary file corrupted during snapshot/restore"

    def test_snapshot_deduplication(self, server):
        """Verify deduplication works (same content = same storage)"""
        daemon_id = "daemon-rocky-1"

        # Create workspace with duplicate content
        server.exec(daemon_id, "mkdir -p /tmp/dedup-test", timeout=5)
        server.exec(daemon_id, "echo 'same content' > /tmp/dedup-test/file1.txt", timeout=5)
        server.exec(daemon_id, "echo 'same content' > /tmp/dedup-test/file2.txt", timeout=5)
        server.exec(daemon_id, "echo 'same content' > /tmp/dedup-test/file3.txt", timeout=5)

        # Create snapshot
        snapshot_id = server.create_snapshot(
            daemon_id,
            "/tmp/dedup-test",
            message="Dedup test"
        )

        # Get snapshot info
        snapshot = server.get_snapshot(daemon_id, snapshot_id)

        # Total size should be much less than 3x file size (due to deduplication)
        # Each file has "same content\n" (13 bytes), but stored only once
        assert snapshot.file_count == 3
        # Size should be close to 13 bytes (one copy), not 39 bytes (three copies)
        # Allow some overhead for tree structures
        assert snapshot.total_size < 100, f"Expected deduplication, got {snapshot.total_size} bytes"

        # Verify all files restored correctly
        server.restore_snapshot(daemon_id, snapshot_id, "/tmp/restored-dedup")
        for i in range(1, 4):
            result = server.exec(daemon_id, f"cat /tmp/restored-dedup/file{i}.txt", timeout=5)
            assert result.success
            assert "same content" in result.stdout


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
