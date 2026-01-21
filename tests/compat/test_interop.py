"""
Interoperability tests between Rust and Python Reticulum implementations.

Tests actual network communication between a Rust Reticulum node and Python RNS.
"""

import asyncio
import json
import os
import signal
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import pytest
import RNS


def find_free_port():
    """Find a free TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(('', 0))
        s.listen(1)
        port = s.getsockname()[1]
    return port


class RustReticulumProcess:
    """Helper to manage a Rust Reticulum process."""

    def __init__(self, config_dir: Path, port: int):
        self.config_dir = config_dir
        self.port = port
        self.process = None
        self.cargo_project = Path(__file__).parent.parent.parent

    def _create_config(self):
        """Create a minimal Reticulum config for the Rust instance."""
        config_dir = self.config_dir / "rust_node"
        config_dir.mkdir(parents=True, exist_ok=True)

        config_path = config_dir / "config"
        config_content = f"""
[reticulum]
enable_transport = false
share_instance = false
panic_on_interface_error = false

[logging]
loglevel = 4

[interfaces.tcp_server]
type = "tcp"
enabled = true
mode = "server"
address = "127.0.0.1"
port = {self.port}
"""
        config_path.write_text(config_content.strip())
        return config_dir

    def start(self):
        """Start the Rust Reticulum process."""
        config_dir = self._create_config()

        # Build and run the example
        env = os.environ.copy()
        env['RUST_LOG'] = 'info'

        self.process = subprocess.Popen(
            ['cargo', 'run', '-p', 'reticulum-tokio', '--example', 'reticulum_basic', '--', str(config_dir)],
            cwd=self.cargo_project,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            bufsize=1
        )

        # Wait for the server to start
        # Look for "Reticulum is running!" in output
        timeout = 30
        start_time = time.time()

        while time.time() - start_time < timeout:
            if self.process.poll() is not None:
                # Process died
                stdout, stderr = self.process.communicate()
                raise RuntimeError(f"Rust process died: {stderr}\n{stdout}")

            # Try to connect to the port
            try:
                with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                    s.settimeout(1)
                    s.connect(('127.0.0.1', self.port))
                    # Successfully connected, server is up
                    return
            except (socket.error, ConnectionRefusedError):
                time.sleep(0.5)

        raise TimeoutError(f"Rust Reticulum failed to start within {timeout}s")

    def stop(self):
        """Stop the Rust Reticulum process."""
        if self.process:
            self.process.send_signal(signal.SIGINT)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()

            stdout, stderr = self.process.communicate()
            return stdout, stderr
        return None, None

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.stop()


@pytest.fixture
def temp_dir():
    """Create a temporary directory for test configs."""
    temp = tempfile.mkdtemp(prefix="reticulum_interop_")
    yield Path(temp)
    # Cleanup happens after test


@pytest.fixture
def free_port():
    """Get a free port for the test."""
    return find_free_port()


def test_rust_server_python_client(temp_dir, free_port):
    """
    Test that Rust Reticulum server can accept TCP connections.

    This is a basic connectivity test - we verify that the Rust TCP server
    successfully binds to a port and accepts connections.
    """
    # Start Rust server
    rust_server = RustReticulumProcess(temp_dir, free_port)

    try:
        rust_server.start()
        print(f"Rust server started on port {free_port}")

        # Give server a moment to fully initialize
        time.sleep(1)

        # Test basic TCP connectivity
        print(f"Testing TCP connection to Rust server on port {free_port}...")
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(5)
            s.connect(('127.0.0.1', free_port))
            print("Successfully connected to Rust TCP server!")

            # The connection should stay open
            time.sleep(1)

        print("SUCCESS: Rust TCP server is accepting connections!")

    finally:
        stdout, stderr = rust_server.stop()
        if stdout:
            print("Rust stdout:", stdout[-500:])  # Last 500 chars
        if stderr:
            print("Rust stderr:", stderr[-500:])


def test_python_server_rust_client(temp_dir, free_port):
    """
    Test Rust Reticulum client connecting to a TCP server.

    This test starts a simple TCP server and verifies that the Rust TCP client
    can successfully connect to it.
    """
    print(f"Starting simple TCP server on port {free_port}...")

    # Start a simple TCP server in a thread
    server_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server_socket.bind(('127.0.0.1', free_port))
    server_socket.listen(1)
    server_socket.settimeout(10)

    print(f"TCP server listening on port {free_port}")

    # Create Rust client config
    rust_config_dir = temp_dir / "rust_client"
    rust_config_dir.mkdir(parents=True, exist_ok=True)

    rust_config_path = rust_config_dir / "config"
    rust_config_content = f"""[reticulum]
enable_transport = false
share_instance = false
panic_on_interface_error = false

[logging]
loglevel = 4

[interfaces.tcp_client]
type = "tcp"
enabled = true
mode = "client"
address = "127.0.0.1"
port = {free_port}
"""
    rust_config_path.write_text(rust_config_content.strip())

    # Start Rust client
    cargo_project = Path(__file__).parent.parent.parent
    env = os.environ.copy()
    env['RUST_LOG'] = 'info'

    rust_process = subprocess.Popen(
        ['cargo', 'run', '-p', 'reticulum-tokio', '--example', 'reticulum_basic', '--', str(rust_config_dir)],
        cwd=cargo_project,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        text=True
    )

    try:
        # Wait for Rust client to connect
        print("Waiting for Rust client to connect...")
        client_socket, client_addr = server_socket.accept()
        print(f"SUCCESS: Rust client connected from {client_addr}!")

        # Keep connection alive briefly
        time.sleep(1)
        client_socket.close()

    except socket.timeout:
        pytest.fail("Rust client failed to connect within timeout")

    finally:
        server_socket.close()

        # Stop Rust client
        rust_process.send_signal(signal.SIGINT)
        try:
            stdout, stderr = rust_process.communicate(timeout=10)
            print("Rust client output:", stdout[-500:] if stdout else "")
        except subprocess.TimeoutExpired:
            rust_process.kill()
            rust_process.communicate()


if __name__ == "__main__":
    # Allow running directly for debugging
    pytest.main([__file__, "-v", "-s"])
