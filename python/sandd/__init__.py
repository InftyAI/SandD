"""
SandD - High-performance remote command execution system

This package provides a Rust-powered WebSocket server for managing
200+ concurrent daemon connections with support for:
- Command execution
- Interactive shell (PTY)
- File transfer

Example:
    >>> from sandd import Server
    >>> server = Server(host="0.0.0.0", port=8765)
    >>>
    >>> # Execute command
    >>> result = server.execute_command("daemon-1", "ls -la")
    >>> print(result.stdout)
    >>>
    >>> # Start interactive shell
    >>> shell = server.start_shell("daemon-1")
    >>> shell.write(b"ls\\n")
    >>> output = shell.read(timeout=1.0)
    >>>
    >>> # File transfer
    >>> server.upload_file("daemon-1", "/remote/path", data)
    >>> data = server.download_file("daemon-1", "/remote/file")
"""

from typing import Optional, Dict, List
import time

try:
    from ._core import Server as _RustServer, ShellSession, PyCommandResult, PyStats
except ImportError as e:
    raise ImportError(
        "Failed to import Rust extension. "
        "Please build the package with: make install"
    ) from e

__version__ = "0.0.0"
__all__ = [
    "Server",
    "ShellSession",
    "CommandResult",
    "ServerStats",
]


class CommandResult:
    """Result from command execution

    Attributes:
        stdout: Standard output from the command
        stderr: Standard error from the command
        exit_code: Exit code (0 = success)
        duration_ms: Execution duration in milliseconds
    """

    def __init__(self, result: PyCommandResult):
        self._result = result

    @property
    def stdout(self) -> str:
        """Standard output"""
        return self._result.stdout

    @property
    def stderr(self) -> str:
        """Standard error"""
        return self._result.stderr

    @property
    def exit_code(self) -> int:
        """Exit code (0 = success)"""
        return self._result.exit_code

    @property
    def duration_ms(self) -> int:
        """Execution duration in milliseconds"""
        return self._result.duration_ms

    @property
    def success(self) -> bool:
        """Whether the command succeeded (exit_code == 0)"""
        return self.exit_code == 0

    def __repr__(self) -> str:
        return (
            f"CommandResult(exit_code={self.exit_code}, "
            f"duration_ms={self.duration_ms}, "
            f"stdout={len(self.stdout)} bytes, "
            f"stderr={len(self.stderr)} bytes)"
        )


class ServerStats:
    """Server statistics

    Attributes:
        total_daemons: Total number of connected daemons
        by_platform: Daemon count by platform (e.g., {"linux": 150, "darwin": 50})
        oldest_connection_secs: Age of oldest connection in seconds
    """

    def __init__(self, stats: PyStats):
        self._stats = stats

    @property
    def total_daemons(self) -> int:
        """Total connected daemons"""
        return self._stats.total_daemons

    @property
    def by_platform(self) -> Dict[str, int]:
        """Daemon count by platform"""
        return self._stats.by_platform

    @property
    def oldest_connection_secs(self) -> int:
        """Age of oldest connection in seconds"""
        return self._stats.oldest_connection_secs

    def __repr__(self) -> str:
        return (
            f"ServerStats(total={self.total_daemons}, "
            f"platforms={self.by_platform})"
        )


class Server:
    """Sandbox execution server

    High-performance WebSocket server for managing remote daemon connections.
    Built with Rust for efficient handling of 200+ concurrent connections.

    Args:
        host: Bind address (default: "0.0.0.0")
        port: Bind port (default: 8765)

    Example:
        >>> server = Server("0.0.0.0", 8765)
        >>> server.wait_for_daemon("daemon-1", timeout=30)
        >>> result = server.execute_command("daemon-1", "hostname")
        >>> print(result.stdout)
    """

    def __init__(self, host: str = "0.0.0.0", port: int = 8765):
        self._server = _RustServer(host, port)
        self._host = host
        self._port = port

    def execute_command(
        self,
        daemon_id: str,
        command: str,
        timeout_secs: int = 300,
        env: Optional[Dict[str, str]] = None,
        cwd: Optional[str] = None,
    ) -> CommandResult:
        """Execute a command on a daemon

        Args:
            daemon_id: Target daemon ID
            command: Command to execute (shell string)
            timeout_secs: Execution timeout in seconds (default: 300)
            env: Environment variables to set
            cwd: Working directory

        Returns:
            CommandResult with stdout, stderr, exit_code, duration

        Raises:
            ValueError: If daemon not found
            TimeoutError: If command times out
            RuntimeError: If command fails to execute

        Example:
            >>> result = server.execute_command("daemon-1", "ls -la /tmp")
            >>> if result.success:
            ...     print(result.stdout)
        """
        result = self._server.execute_command(
            daemon_id, command, timeout_secs, env, cwd
        )
        return CommandResult(result)

    def start_shell(
        self,
        daemon_id: str,
        rows: int = 24,
        cols: int = 80,
        term: str = "xterm-256color",
    ) -> ShellSession:
        """Start an interactive shell session

        Args:
            daemon_id: Target daemon ID
            rows: Terminal rows (default: 24)
            cols: Terminal columns (default: 80)
            term: TERM environment variable (default: "xterm-256color")

        Returns:
            ShellSession for interactive I/O

        Raises:
            ValueError: If daemon not found
            RuntimeError: If shell fails to start

        Example:
            >>> shell = server.start_shell("daemon-1")
            >>> shell.write(b"ls -la\\n")
            >>> output = shell.read(timeout=1.0)
            >>> if output:
            ...     print(output.decode())
        """
        return self._server.start_shell(daemon_id, rows, cols, term)

    def upload_file(
        self,
        daemon_id: str,
        remote_path: str,
        data: bytes,
    ) -> None:
        """Upload a file to a daemon

        Args:
            daemon_id: Target daemon ID
            remote_path: Destination path on daemon
            data: File data to upload

        Raises:
            ValueError: If daemon not found
            RuntimeError: If upload fails

        Example:
            >>> with open("config.yaml", "rb") as f:
            ...     data = f.read()
            >>> server.upload_file("daemon-1", "/etc/app/config.yaml", data)
        """
        self._server.upload_file(daemon_id, remote_path, data)

    def download_file(
        self,
        daemon_id: str,
        remote_path: str,
    ) -> bytes:
        """Download a file from a daemon

        Args:
            daemon_id: Target daemon ID
            remote_path: Source path on daemon

        Returns:
            File data as bytes

        Raises:
            ValueError: If daemon not found
            RuntimeError: If download fails

        Example:
            >>> data = server.download_file("daemon-1", "/var/log/app.log")
            >>> with open("app.log", "wb") as f:
            ...     f.write(data)
        """
        return self._server.download_file(daemon_id, remote_path)

    def list_daemons(self) -> List[str]:
        """List all connected daemon IDs

        Returns:
            List of daemon IDs

        Example:
            >>> daemons = server.list_daemons()
            >>> print(f"Connected: {len(daemons)} daemons")
            >>> for daemon_id in daemons:
            ...     print(f"  - {daemon_id}")
        """
        return self._server.list_daemons()

    def daemon_count(self) -> int:
        """Get number of connected daemons

        Returns:
            Count of connected daemons
        """
        return self._server.daemon_count()

    def get_stats(self) -> ServerStats:
        """Get server statistics

        Returns:
            ServerStats with connection metrics

        Example:
            >>> stats = server.get_stats()
            >>> print(f"Total: {stats.total_daemons}")
            >>> print(f"Platforms: {stats.by_platform}")
        """
        return ServerStats(self._server.get_stats())

    def wait_for_daemon(
        self,
        daemon_id: str,
        timeout: float = 30.0,
        poll_interval: float = 0.5,
    ) -> bool:
        """Wait for a daemon to connect

        Args:
            daemon_id: Daemon ID to wait for
            timeout: Maximum wait time in seconds
            poll_interval: How often to check (seconds)

        Returns:
            True if daemon connected, False if timed out

        Example:
            >>> if server.wait_for_daemon("daemon-1", timeout=60):
            ...     print("Daemon connected!")
            ...     result = server.execute_command("daemon-1", "hostname")
            ... else:
            ...     print("Timeout waiting for daemon")
        """
        start = time.time()
        while time.time() - start < timeout:
            if daemon_id in self.list_daemons():
                return True
            time.sleep(poll_interval)
        return False

    @property
    def address(self) -> str:
        """Server address (host:port)"""
        return f"{self._host}:{self._port}"

    def __repr__(self) -> str:
        return (
            f"Server(address={self.address}, "
            f"daemons={self.daemon_count()})"
        )
