"""Data models for SandD"""

from typing import Dict, List
from datetime import datetime

try:
    from ._core import PyCommandResult, PyStats
except ImportError as e:
    raise ImportError(
        "Failed to import Rust extension. Please build the package with: make install"
    ) from e


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


class DaemonInfo:
    """Information about a connected daemon

    Attributes:
        id: Daemon identifier
        version: Daemon version string
        labels: Key-value labels for filtering
        is_busy: Whether daemon has pending commands
    """

    def __init__(
        self,
        id: str,
        version: str,
        labels: Dict[str, str],
        is_busy: bool,
    ):
        self.id = id
        self.version = version
        self.labels = labels
        self.is_busy = is_busy

    def __repr__(self) -> str:
        return (
            f"DaemonInfo(id={self.id!r}, version={self.version!r}, "
            f"labels={self.labels}, is_busy={self.is_busy})"
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
        return f"ServerStats(total={self.total_daemons}, platforms={self.by_platform})"


class SnapshotInfo:
    """Snapshot metadata

    Attributes:
        id: Snapshot ID (UUID)
        created_at: Creation timestamp
        message: Snapshot description
        tags: List of tags (immutable)
        file_count: Number of files in snapshot
        total_size: Total size in bytes
    """

    def __init__(
        self,
        id: str,
        created_at: int,  # Unix timestamp
        message: str,
        tags: List[str],
        file_count: int,
        total_size: int,
    ):
        self.id = id
        self.created_at = datetime.fromtimestamp(created_at)
        self.message = message
        self.tags = tags
        self.file_count = file_count
        self.total_size = total_size

    def __repr__(self) -> str:
        return (
            f"SnapshotInfo(id={self.id!r}, message={self.message!r}, "
            f"tags={self.tags}, files={self.file_count})"
        )
