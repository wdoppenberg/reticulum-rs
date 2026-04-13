"""
Shared helpers for Reticulum Rust↔Python compatibility tests.

The central fixture is `RustNode`, a context manager that:
1. Builds and starts the `compat_node` binary on a free port.
2. Parses the structured ``READY port=... dest=...`` line from its stdout.
3. Exposes the port and destination hash so tests can connect Python RNS.
4. Tears down the process cleanly on exit.
"""

from __future__ import annotations

import os
import signal
import socket
import subprocess
import time
from pathlib import Path
from typing import Optional


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

CARGO_ROOT: Path = Path(__file__).parent.parent.parent
"""Absolute path to the workspace root (contains the top-level Cargo.toml)."""

READY_TIMEOUT_S: float = 60.0
"""Seconds to wait for the Rust node to print its READY line."""

CONNECT_TIMEOUT_S: float = 5.0
"""Seconds for a TCP probe to confirm the port is accepting connections."""


def find_free_port() -> int:
    """Return an OS-assigned free TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        s.listen(1)
        return s.getsockname()[1]


def wait_for_port(host: str, port: int, timeout: float = CONNECT_TIMEOUT_S) -> bool:
    """Probe `host:port` until it accepts a TCP connection or `timeout` expires."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return True
        except OSError:
            time.sleep(0.25)
    return False


# ---------------------------------------------------------------------------
# RustNode
# ---------------------------------------------------------------------------

class RustNode:
    """
    Manage a ``compat_node`` subprocess for integration tests.

    Usage::

        with RustNode() as node:
            assert node.port > 0
            assert len(node.dest_hash) == 32  # 16-byte hash → 32 hex chars

    The node is built with ``cargo run`` the first time; subsequent runs in the
    same session benefit from the incremental build cache.
    """

    def __init__(
        self,
        port: Optional[int] = None,
        config_dir: Optional[Path] = None,
        announce_interval: int = 3,
        extra_env: Optional[dict] = None,
    ) -> None:
        self.port: int = port or find_free_port()
        self.config_dir: Path = config_dir or (
            Path(os.getenv("TMPDIR", "/tmp")) / f"rns_compat_{self.port}"
        )
        self.announce_interval = announce_interval
        self.extra_env = extra_env or {}

        self.dest_hash: str = ""
        self._process: Optional[subprocess.Popen] = None

    # ── Context manager ───────────────────────────────────────────────────

    def __enter__(self) -> "RustNode":
        self.start()
        return self

    def __exit__(self, *_) -> None:
        self.stop()

    # ── Lifecycle ─────────────────────────────────────────────────────────

    def start(self) -> None:
        """Build (if necessary) and launch the compat_node binary."""
        env = {**os.environ, "RUST_LOG": "info", **self.extra_env}

        self._process = subprocess.Popen(
            [
                "cargo", "run",
                "--quiet",
                "-p", "reticulum-tokio",
                "--example", "compat_node",
                "--",
                "--port", str(self.port),
                "--config-dir", str(self.config_dir),
                "--announce-interval", str(self.announce_interval),
            ],
            cwd=str(CARGO_ROOT),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            bufsize=1,          # line-buffered
        )

        self.dest_hash = self._wait_for_ready()

    def stop(self) -> tuple[str, str]:
        """Send SIGINT, wait up to 10 s, then SIGKILL."""
        if self._process is None:
            return "", ""
        try:
            self._process.send_signal(signal.SIGINT)
            stdout, stderr = self._process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            self._process.kill()
            stdout, stderr = self._process.communicate()
        finally:
            self._process = None
        return stdout or "", stderr or ""

    # ── Internals ─────────────────────────────────────────────────────────

    def _wait_for_ready(self) -> str:
        """
        Read stdout until we see ``READY port=... dest=...``.

        Returns the destination hash (hex string) on success.
        Raises ``TimeoutError`` if the node does not become ready in time.
        Raises ``RuntimeError`` if the process dies unexpectedly.
        """
        assert self._process is not None
        assert self._process.stdout is not None

        deadline = time.monotonic() + READY_TIMEOUT_S

        while time.monotonic() < deadline:
            # Check for early exit.
            retcode = self._process.poll()
            if retcode is not None:
                stderr = self._process.stderr.read() if self._process.stderr else ""
                raise RuntimeError(
                    f"compat_node exited early with code {retcode}.\n"
                    f"stderr:\n{stderr[-2000:]}"
                )

            line = self._process.stdout.readline()
            if not line:
                time.sleep(0.05)
                continue

            line = line.strip()
            if line.startswith("READY "):
                return self._parse_ready(line)

        self.stop()
        raise TimeoutError(
            f"compat_node did not print READY within {READY_TIMEOUT_S}s"
        )

    @staticmethod
    def _parse_ready(line: str) -> str:
        """
        Parse ``READY port=19991 dest=aabbcc…`` and return the dest hash.
        """
        parts = dict(
            kv.split("=", 1)
            for kv in line.split()
            if "=" in kv
        )
        dest = parts.get("dest", "")
        if not dest:
            raise ValueError(f"Could not parse dest from READY line: {line!r}")
        return dest

    # ── Convenience ───────────────────────────────────────────────────────

    def tcp_address(self) -> tuple[str, int]:
        """Return ``(host, port)`` for Python RNS TCP client config."""
        return ("127.0.0.1", self.port)

    def wait_for_port(self, timeout: float = CONNECT_TIMEOUT_S) -> bool:
        """Return True once the TCP server is accepting connections."""
        return wait_for_port("127.0.0.1", self.port, timeout)
