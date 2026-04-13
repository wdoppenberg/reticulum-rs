"""
Pytest configuration and shared fixtures for Reticulum compatibility tests.
"""

from __future__ import annotations

import os
import shutil
import tempfile
from pathlib import Path
from typing import Generator

import RNS
import pytest

from helpers import RustNode, find_free_port


# ---------------------------------------------------------------------------
# Session-scoped Python RNS instance
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def rns_instance():
    """
    A minimal Python RNS instance, shared across all tests in the session.

    No interfaces are attached; tests that need an interface start their own.
    """
    temp_dir = tempfile.mkdtemp(prefix="rns_session_")
    try:
        rns = RNS.Reticulum(
            configdir=temp_dir,
            loglevel=RNS.LOG_ERROR,
        )
        yield rns
    finally:
        try:
            shutil.rmtree(temp_dir)
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Per-test temp directory
# ---------------------------------------------------------------------------

@pytest.fixture()
def tmp(tmp_path: Path) -> Path:
    """A unique temporary directory for each test."""
    return tmp_path


# ---------------------------------------------------------------------------
# Free-port fixture
# ---------------------------------------------------------------------------

@pytest.fixture()
def free_port() -> int:
    """An OS-assigned free TCP port, unique per test."""
    return find_free_port()


# ---------------------------------------------------------------------------
# RustNode fixture
# ---------------------------------------------------------------------------

@pytest.fixture()
def rust_node(tmp_path: Path) -> Generator[RustNode, None, None]:
    """
    Start a Rust `compat_node` on a free port and yield it to the test.

    The node is stopped automatically after the test completes (pass or fail).

    Example::

        def test_something(rust_node):
            host, port = rust_node.tcp_address()
            # connect Python RNS as a TCP client to (host, port)
            assert rust_node.dest_hash != ""
    """
    node = RustNode(config_dir=tmp_path / "rust_node")
    node.start()
    try:
        yield node
    finally:
        node.stop()


# ---------------------------------------------------------------------------
# Python-RNS-over-TCP fixture
# ---------------------------------------------------------------------------

@pytest.fixture()
def rns_connected_to(tmp_path: Path):
    """
    Factory fixture: given a ``(host, port)``, return a Python RNS instance
    connected to that TCP endpoint as a client.

    Usage::

        def test_foo(rust_node, rns_connected_to):
            rns = rns_connected_to(rust_node.tcp_address())
            # rns is now connected to the Rust node via TCP
    """
    instances: list[tuple[RNS.Reticulum, str]] = []

    def _factory(address: tuple[str, int]) -> RNS.Reticulum:
        host, port = address
        cfg_dir = tempfile.mkdtemp(
            prefix="rns_client_", dir=str(tmp_path)
        )
        rns = RNS.Reticulum(configdir=cfg_dir, loglevel=RNS.LOG_ERROR)
        RNS.Interface.TCPClientInterface(
            RNS.Reticulum.Transport,
            "tcp_to_rust",
            target_host=host,
            target_port=port,
        )
        instances.append((rns, cfg_dir))
        return rns

    yield _factory

    # Cleanup
    for _rns, cfg_dir in instances:
        try:
            shutil.rmtree(cfg_dir)
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Test vectors directory
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def vectors_dir() -> Path:
    """Directory for storing test vector JSON files."""
    d = Path(__file__).parent / "vectors"
    d.mkdir(exist_ok=True)
    return d
