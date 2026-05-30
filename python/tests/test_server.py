"""Unit tests for SandD Server"""
import pytest
from sandd import Server, ServerStats


def test_server_initialization():
    """Test server can be initialized with default parameters"""
    server = Server()
    assert server.address == "0.0.0.0:8765"
    assert server.daemon_count() == 0


def test_server_custom_address():
    """Test server can be initialized with custom host and port"""
    server = Server(host="127.0.0.1", port=9999)
    assert server.address == "127.0.0.1:9999"


def test_list_daemons_empty():
    """Test listing daemons returns empty list when none connected"""
    server = Server()
    daemons = server.list_daemons()
    assert isinstance(daemons, list)
    assert len(daemons) == 0


def test_execute_command_daemon_not_found():
    """Test executing command on non-existent daemon raises ValueError"""
    server = Server()
    with pytest.raises(ValueError, match="not found"):
        server.execute_command("non-existent-daemon", "echo test")


def test_wait_for_daemon_timeout():
    """Test wait_for_daemon returns False on timeout"""
    server = Server()
    result = server.wait_for_daemon("non-existent", timeout=0.1, poll_interval=0.05)
    assert result is False


def test_server_repr():
    """Test server string representation"""
    server = Server(host="localhost", port=8080)
    repr_str = repr(server)
    assert "localhost:8080" in repr_str
    assert "daemons=0" in repr_str


def test_get_stats():
    """Test getting server statistics"""
    server = Server()
    stats = server.get_stats()
    assert isinstance(stats, ServerStats)
    assert stats.total_daemons == 0
    assert isinstance(stats.by_platform, dict)
