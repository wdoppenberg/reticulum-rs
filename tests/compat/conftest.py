"""
Pytest configuration for Reticulum compatibility tests.
"""

import RNS
import pytest
import tempfile
import shutil
from pathlib import Path


@pytest.fixture(scope="session")
def rns_instance():
    """
    Create a Reticulum instance for tests.

    This fixture is session-scoped so the RNS instance is shared across all tests.
    """
    # Create temporary directory for RNS storage
    temp_dir = tempfile.mkdtemp(prefix="rns_test_")

    try:
        # Initialize Reticulum with minimal configuration
        # Using None as config path creates a minimal instance
        reticulum = RNS.Reticulum(
            configdir=temp_dir,
            loglevel=RNS.LOG_ERROR  # Suppress logs during tests
        )

        yield reticulum

    finally:
        # Cleanup
        try:
            shutil.rmtree(temp_dir)
        except:
            pass


@pytest.fixture(scope="session")
def test_vectors_dir():
    """Get or create the test vectors directory."""
    vectors_dir = Path(__file__).parent / "vectors"
    vectors_dir.mkdir(exist_ok=True)
    return vectors_dir
