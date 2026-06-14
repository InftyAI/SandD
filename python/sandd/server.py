"""Sync API for SandD server"""

from typing import Optional, Dict, List
import time
import sys
import select

from .models import CommandResult, ServerStats

try:
    from ._core import Server as _RustServer, Session
except ImportError as e:
    raise ImportError(
        "Failed to import Rust extension. "
        "Please build the package with: make install"
    ) from e


class Server:
    """Sandbox execution server

    High-performance WebSocket server for managing remote daemon connections.
    Built with Rust for efficient handling of high-concurrency workloads.

    Args:
        host: Bind address (default: "0.0.0.0")
        port: Bind port (default: 8765)
        verbose: Enable logging at INFO level (default: True)
                Set to False to disable logs (useful for interactive sessions)

    Example:
        >>> server = Server("0.0.0.0", 8765)
        >>> server.wait_for_daemon("daemon-1", timeout=30)
        >>> result = server.exec("daemon-1", "hostname")
        >>> print(result.stdout)

        >>> # Disable logs for clean output, useful for interactive sessions
        >>> server = Server("0.0.0.0", 8765, verbose=False)
    """

    def __init__(self, host: str = "0.0.0.0", port: int = 8765, verbose: bool = True):
        self._server = _RustServer(host, port, verbose)
        self._host = host
        self._port = port

    def exec(
        self,
        daemon_id: str,
        command: str,
        timeout: int = 300,
        env: Optional[Dict[str, str]] = None,
        cwd: Optional[str] = None,
    ) -> CommandResult:
        """Execute a command on a daemon

        Commands are processed sequentially by each daemon. If multiple commands
        are sent to the same daemon, they will queue and execute one at a time.

        Args:
            daemon_id: Target daemon ID
            command: Command to execute (shell string)
            timeout: Execution timeout in seconds (default: 300)
            env: Environment variables to set
            cwd: Working directory

        Returns:
            CommandResult with stdout, stderr, exit_code, duration

        Raises:
            ValueError: If daemon not found
            TimeoutError: If command times out
            RuntimeError: If command fails to execute

        Example:
            >>> # Single command
            >>> result = server.exec("daemon-1", "ls -la /tmp")
            >>> if result.success:
            ...     print(result.stdout)
            >>>
            >>> # Multiple commands to same daemon execute sequentially
            >>> result1 = server.exec("daemon-1", "sleep 5")  # Takes 5s
            >>> result2 = server.exec("daemon-1", "echo hi")   # Waits for first to finish

        Note:
            Each daemon processes commands sequentially to ensure predictable
            execution order and avoid resource conflicts.
        """
        result = self._server.exec(
            daemon_id, command, timeout, env, cwd
        )
        return CommandResult(result)

    def new_session(
        self,
        daemon_id: str,
        rows: int = 24,
        cols: int = 80,
        term: str = "xterm-256color",
        interactive: bool = False,
    ) -> Session:
        """Create a new interactive session

        Args:
            daemon_id: Target daemon ID
            rows: Terminal rows (default: 24)
            cols: Terminal columns (default: 80)
            term: TERM environment variable (default: "xterm-256color")
            interactive: If True, enters interactive mode with live terminal (default: False)

        Returns:
            Session for interactive I/O (or None if interactive=True, runs in foreground)

        Raises:
            ValueError: If daemon not found
            RuntimeError: If session fails to start

        Example (Programmatic):
            >>> session = server.new_session("daemon-1")
            >>> session.write(b"ls -la\\n")
            >>> output = session.read(timeout=1.0)
            >>> if output:
            ...     print(output.decode())

        Example (Interactive):
            >>> server.new_session("daemon-1", interactive=True)
            # Enters interactive terminal session - type commands directly
        """
        session = self._server.new_session(daemon_id, rows, cols, term)

        if interactive:
            self._run_interactive(session)
            return None

        return session

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

    def list_daemons(
        self,
        labels: Optional[Dict[str, str]] = None,
    ) -> List[str]:
        """List all connected daemon IDs, optionally filtered by labels

        Args:
            labels: Dictionary of label key-value pairs to filter by (AND logic)
                   All specified labels must match for a daemon to be included

        Returns:
            List of daemon IDs

        Example:
            >>> # List all daemons
            >>> daemons = server.list_daemons()
            >>> print(f"Connected: {len(daemons)} daemons")
            >>>
            >>> # List daemons with single label
            >>> prod_daemons = server.list_daemons(labels={"env": "prod"})
            >>>
            >>> # List daemons with multiple labels (AND logic)
            >>> west_prod = server.list_daemons(labels={"env": "prod", "region": "us-west"})
            >>> for daemon_id in west_prod:
            ...     print(f"  - {daemon_id}")
        """
        return self._server.list_daemons(labels)

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

    def _run_interactive(self, session: Session) -> None:
        """Run session in interactive mode with live terminal

        Args:
            session: Session to make interactive
        """
        print("Entering interactive session. Press Ctrl+D to exit.")
        print("-" * 60)

        # Set terminal to raw mode on Unix systems
        if sys.platform != "win32":
            import tty
            import termios
            old_settings = termios.tcgetattr(sys.stdin)
            try:
                tty.setraw(sys.stdin.fileno())
                self._interactive_loop(session)
            finally:
                termios.tcsetattr(sys.stdin, termios.TCSADRAIN, old_settings)
        else:
            # Windows - just run without raw mode
            self._interactive_loop(session)

        print("\n" + "-" * 60)
        print("Interactive session ended.")
        session.close()

    def _interactive_loop(self, session: Session) -> None:
        """Main interactive I/O loop

        Args:
            session: Session for I/O
        """
        try:
            while True:
                # Check for input from stdin
                if sys.platform != "win32":
                    rlist, _, _ = select.select([sys.stdin], [], [], 0.01)
                    if rlist:
                        data = sys.stdin.read(1)
                        if not data or data == '\x04':  # Ctrl+D
                            break
                        session.write(data.encode())
                else:
                    # Windows - simple blocking read
                    import msvcrt
                    if msvcrt.kbhit():
                        data = msvcrt.getch()
                        if data == b'\x04':  # Ctrl+D
                            break
                        session.write(data)

                # Read output from session
                output = session.read(timeout=0.01)
                if output:
                    sys.stdout.buffer.write(output)
                    sys.stdout.buffer.flush()

        except KeyboardInterrupt:
            # Ctrl+C - exit gracefully
            pass

    def broadcast(
        self,
        labels: Dict[str, str],
        command: str,
        timeout: int = 300,
        env: Optional[Dict[str, str]] = None,
        cwd: Optional[str] = None,
    ) -> Dict[str, CommandResult]:
        """Broadcast a command to all daemons matching labels

        Executes the same command on all matching daemons concurrently using
        Python threads. All daemons receive and execute the command in parallel,
        making this much faster than calling exec() in a loop.

        Args:
            labels: Label filters (all must match, AND logic)
            command: Command to execute on all matching daemons
            timeout: Execution timeout in seconds (default: 300)
            env: Environment variables to set
            cwd: Working directory

        Returns:
            Dict mapping daemon_id -> CommandResult

        Example:
            >>> # Update all production workers concurrently
            >>> results = server.broadcast(
            ...     labels={"env": "prod", "role": "worker"},
            ...     command="git pull && systemctl restart app"
            ... )
            >>>
            >>> # Check results
            >>> for daemon_id, result in results.items():
            ...     if result.success:
            ...         print(f"{daemon_id}: OK")
            ...     else:
            ...         print(f"{daemon_id}: FAILED - {result.stderr}")

        Performance:
            Broadcasting to N daemons takes approximately the same time as
            executing on a single daemon (all run in parallel), rather than
            N times longer (sequential execution).
        """
        import concurrent.futures

        # Get matching daemons
        daemon_ids = self.list_daemons(labels=labels)
        if not daemon_ids:
            return {}

        # Execute command on all daemons concurrently
        def run_command(daemon_id):
            try:
                return self.exec(daemon_id, command, timeout, env, cwd)
            except Exception as e:
                # Create error result
                return CommandResult(type('obj', (object,), {
                    'stdout': '',
                    'stderr': str(e),
                    'exit_code': -1,
                    'duration_ms': 0,
                })())

        results = {}
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(daemon_ids)) as executor:
            futures = {executor.submit(run_command, did): did for did in daemon_ids}
            for future in concurrent.futures.as_completed(futures):
                daemon_id = futures[future]
                results[daemon_id] = future.result()

        return results

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
            ...     result = server.exec("daemon-1", "hostname")
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
