"""
SandD - High-performance remote command execution system

This package provides a Rust-powered WebSocket server for managing
high-concurrency daemon connections with support for:
- Command execution
- Interactive session (PTY)
- File transfer

Example (Sync API):
    >>> from sandd import Server
    >>> server = Server(host="0.0.0.0", port=8765)
    >>>
    >>> # Execute command
    >>> result = server.exec("daemon-1", "ls -la")
    >>> print(result.stdout)
    >>>
    >>> # Start interactive session
    >>> session = server.new_session("daemon-1")
    >>> session.write(b"ls\\n")
    >>> output = session.read(timeout=1.0)
    >>>
    >>> # File transfer
    >>> server.upload_file("daemon-1", "/remote/path", data)
    >>> data = server.download_file("daemon-1", "/remote/file")

Example (Async API - Not Yet Implemented):
    >>> from sandd import AsyncServer
    >>> server = AsyncServer(host="0.0.0.0", port=8765)
    >>>
    >>> # Execute command
    >>> result = await server.exec("daemon-1", "ls -la")
    >>> print(result.stdout)
    >>>
    >>> # Concurrent execution
    >>> results = await asyncio.gather(
    ...     server.exec("daemon-1", "hostname"),
    ...     server.exec("daemon-2", "uptime")
    ... )
"""

from .models import CommandResult, ServerStats, DaemonInfo
from .server import Server
from .async_server import AsyncServer
from .keys import tunnel_config_from_env

try:
    from ._core import Session, TunnelConfig
except ImportError as e:
    raise ImportError(
        "Failed to import Rust extension. Please build the package with: make install"
    ) from e

__all__ = [
    "Server",
    "AsyncServer",
    "Session",
    "CommandResult",
    "ServerStats",
    "DaemonInfo",
    "TunnelConfig",
    "tunnel_config_from_env",
]
