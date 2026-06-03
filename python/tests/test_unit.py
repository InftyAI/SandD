"""Comprehensive unit tests for SandD Python API

These tests verify the Python API without requiring real daemon connections.
For integration tests with real daemons, see test_integration.py
"""
import pytest
from sandd import Server, ServerStats


class TestServerAPI:
    """Test Server class initialization and properties"""

    def test_default_init(self):
        """Test server can be initialized with default parameters"""
        server = Server()
        assert server.address == "0.0.0.0:8765"
        assert server.daemon_count() == 0

    def test_custom_host_port(self):
        """Test server can be initialized with custom host and port"""
        server = Server(host="127.0.0.1", port=9000)
        assert server.address == "127.0.0.1:9000"

    def test_custom_host_port_alternative(self):
        """Test another custom address configuration"""
        server = Server(host="127.0.0.1", port=9999)
        assert server.address == "127.0.0.1:9999"

    def test_address_property(self):
        """Test server address property returns correct format"""
        server = Server(host="192.168.1.1", port=5000)
        assert server.address == "192.168.1.1:5000"

    def test_repr(self):
        """Test server string representation"""
        server = Server(host="localhost", port=8080)
        repr_str = repr(server)
        assert "localhost:8080" in repr_str
        assert "daemons=" in repr_str
        assert "daemons=0" in repr_str


class TestListDaemons:
    """Test list_daemons method"""

    def test_returns_list(self):
        server = Server()
        result = server.list_daemons()
        assert isinstance(result, list)

    def test_empty_when_no_daemons(self):
        server = Server()
        assert server.list_daemons() == []

    def test_with_label_filters(self):
        server = Server()
        result = server.list_daemons(label_key="env", label_value="prod")
        assert isinstance(result, list)

    def test_with_partial_filters(self):
        server = Server()
        assert isinstance(server.list_daemons(label_key="env"), list)
        assert isinstance(server.list_daemons(label_value="prod"), list)


class TestDaemonCount:
    """Test daemon_count method"""

    def test_returns_int(self):
        server = Server()
        count = server.daemon_count()
        assert isinstance(count, int)

    def test_zero_when_empty(self):
        server = Server()
        assert server.daemon_count() == 0


class TestServerStats:
    """Test get_stats method"""

    def test_returns_stats_object(self):
        server = Server()
        stats = server.get_stats()
        assert isinstance(stats, ServerStats)

    def test_stats_properties(self):
        server = Server()
        stats = server.get_stats()
        assert isinstance(stats.total_daemons, int)
        assert isinstance(stats.by_platform, dict)
        assert isinstance(stats.oldest_connection_secs, int)

    def test_stats_repr(self):
        server = Server()
        stats = server.get_stats()
        repr_str = repr(stats)
        assert "ServerStats" in repr_str
        assert "total=" in repr_str


class TestErrorHandling:
    """Test error handling"""

    def test_exec_invalid_daemon(self):
        server = Server()
        with pytest.raises(ValueError, match="not found"):
            server.exec("invalid", "echo test")

    def test_session_invalid_daemon(self):
        server = Server()
        with pytest.raises(ValueError, match="not found"):
            server.new_session("invalid")

    def test_upload_file_invalid_daemon(self):
        server = Server()
        with pytest.raises(ValueError, match="not found"):
            server.upload_file("invalid", "/tmp/test", b"data")

    def test_download_file_invalid_daemon(self):
        server = Server()
        with pytest.raises(ValueError, match="not found"):
            server.download_file("invalid", "/tmp/test")


class TestWaitForDaemon:
    """Test wait_for_daemon method"""

    def test_returns_bool(self):
        server = Server()
        result = server.wait_for_daemon("test", timeout=0.01)
        assert isinstance(result, bool)

    def test_timeout_returns_false(self):
        server = Server()
        result = server.wait_for_daemon("nonexistent", timeout=0.1, poll_interval=0.05)
        assert result is False
