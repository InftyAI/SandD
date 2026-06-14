"""Data models for SandD"""

from typing import Dict

try:
    from ._core import PyCommandResult, PyStats
except ImportError as e:
    raise ImportError(
        "Failed to import Rust extension. "
        "Please build the package with: make install"
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
