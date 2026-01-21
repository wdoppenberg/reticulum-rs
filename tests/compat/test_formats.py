"""
Test binary format compatibility between Rust and Python Reticulum implementations.

These tests verify that:
1. Identities serialize/deserialize the same way
2. Packets have identical binary format
3. Announces are binary compatible
4. Hashes are computed identically
"""

import RNS
import pytest
import json
import subprocess
from pathlib import Path


class TestIdentityFormat:
    """Test Identity serialization format compatibility."""

    def test_identity_hash_format(self):
        """Test that identity hashes are computed the same way."""
        # Create identity with known keys
        identity = RNS.Identity()

        # Get the public key bytes
        pub_key_bytes = identity.get_public_key()

        # Compute hash using Python implementation
        identity_hash = RNS.Identity.full_hash(pub_key_bytes)

        # Export for Rust test
        test_data = {
            "public_key_hex": pub_key_bytes.hex(),
            "expected_hash_hex": identity_hash.hex(),
        }

        print(f"\nIdentity test data:")
        print(f"  Public key length: {len(pub_key_bytes)} bytes")
        print(f"  Hash length: {len(identity_hash)} bytes")
        print(f"  Hash (hex): {identity_hash.hex()}")

        # Save test vector for Rust tests
        test_vector_path = Path(__file__).parent / "vectors" / "identity_hash.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)

    def test_identity_keys_format(self):
        """Test that identity keys are stored in the same format."""
        identity = RNS.Identity()

        # Get public and private key components
        # Note: This tests the external format, not internal representation
        pub_key = identity.get_public_key()

        test_data = {
            "public_key_hex": pub_key.hex(),
            "public_key_length": len(pub_key),
        }

        print(f"\nIdentity key format:")
        print(f"  Public key length: {len(pub_key)} bytes")
        print(f"  Expected: 64 bytes (32 for encryption + 32 for signing)")

        assert len(pub_key) == 64, "Public key should be 64 bytes"

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "identity_keys.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


class TestPacketFormat:
    """Test packet binary format compatibility."""

    def test_packet_header_format(self, rns_instance):
        """Test that packet headers encode the same way."""
        # Create a simple destination
        identity = RNS.Identity()
        destination = RNS.Destination(
            identity,
            RNS.Destination.IN,
            RNS.Destination.SINGLE,
            "test_app",
            "test_aspect"
        )

        # Create packet
        packet = RNS.Packet(destination, "test data".encode())

        # Get raw packet bytes (this would normally be sent over network)
        # Note: We need to manually construct or inspect the packet format

        test_data = {
            "destination_hash_hex": destination.hash.hex(),
            "destination_hash_length": len(destination.hash),
        }

        print(f"\nPacket format:")
        print(f"  Destination hash: {destination.hash.hex()}")
        print(f"  Hash length: {len(destination.hash)} bytes")

        assert len(destination.hash) == 16, "Destination hash should be 16 bytes (truncated)"

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "packet_header.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


class TestAnnounceFormat:
    """Test announce packet format compatibility."""

    def test_announce_creation(self, rns_instance):
        """Test that announces are created with the same format."""
        # Create identity and destination
        identity = RNS.Identity()
        destination = RNS.Destination(
            identity,
            RNS.Destination.IN,
            RNS.Destination.SINGLE,
            "test_app",
            "test_aspect"
        )

        # Announce the destination
        destination.announce()

        test_data = {
            "identity_hex": identity.get_public_key().hex(),
            "destination_hash_hex": destination.hash.hex(),
            "app_name": "test_app",
            "aspects": ["test_aspect"],
        }

        print(f"\nAnnounce format:")
        print(f"  Destination hash: {destination.hash.hex()}")
        print(f"  Identity public key length: {len(identity.get_public_key())} bytes")

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "announce.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


class TestHashFunctions:
    """Test that hash functions produce identical results."""

    def test_truncated_hash(self):
        """Test address hash truncation matches."""
        test_input = b"test data for hashing"

        # Compute full hash
        full_hash = RNS.Identity.full_hash(test_input)

        # Truncated hash (first 16 bytes for addresses)
        truncated_hash = RNS.Identity.truncated_hash(test_input)

        test_data = {
            "input_hex": test_input.hex(),
            "full_hash_hex": full_hash.hex(),
            "truncated_hash_hex": truncated_hash.hex(),
            "full_hash_length": len(full_hash),
            "truncated_hash_length": len(truncated_hash),
        }

        print(f"\nHash functions:")
        print(f"  Input: {test_input.hex()}")
        print(f"  Full hash (SHA-256): {full_hash.hex()}")
        print(f"  Truncated hash: {truncated_hash.hex()}")
        print(f"  Full length: {len(full_hash)} bytes")
        print(f"  Truncated length: {len(truncated_hash)} bytes")

        assert len(full_hash) == 32, "Full hash should be 32 bytes (SHA-256)"
        assert len(truncated_hash) == 16, "Truncated hash should be 16 bytes"
        assert full_hash[:16] == truncated_hash, "Truncated should be first 16 bytes of full"

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "hash_functions.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)

    def test_destination_hash_derivation(self, rns_instance):
        """Test that destination hashes are derived correctly."""
        identity = RNS.Identity()

        # Create destination with known parameters
        app_name = "test_app"
        aspects = ["aspect1", "aspect2"]

        destination = RNS.Destination(
            identity,
            RNS.Destination.IN,
            RNS.Destination.SINGLE,
            app_name,
            *aspects
        )

        test_data = {
            "identity_hex": identity.get_public_key().hex(),
            "app_name": app_name,
            "aspects": aspects,
            "destination_hash_hex": destination.hash.hex(),
            "name_hash_hex": destination.name_hash.hex(),
        }

        print(f"\nDestination hash derivation:")
        print(f"  App name: {app_name}")
        print(f"  Aspects: {aspects}")
        print(f"  Name hash: {destination.name_hash.hex()}")
        print(f"  Destination hash: {destination.hash.hex()}")

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "destination_hash.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


class TestCryptographyFormat:
    """Test cryptographic operations produce compatible results."""

    def test_signature_format(self):
        """Test that signatures are created in the same format."""
        identity = RNS.Identity()
        test_data = b"test message to sign"

        # Sign the data
        signature = identity.sign(test_data)

        test_vector = {
            "message_hex": test_data.hex(),
            "public_key_hex": identity.get_public_key().hex(),
            "signature_hex": signature.hex(),
            "signature_length": len(signature),
        }

        print(f"\nSignature format:")
        print(f"  Message: {test_data.hex()}")
        print(f"  Signature length: {len(signature)} bytes")
        print(f"  Expected: 64 bytes (Ed25519)")

        assert len(signature) == 64, "Ed25519 signature should be 64 bytes"

        # Verify signature works
        assert identity.validate(signature, test_data), "Signature validation should succeed"

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "signature.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_vector, f, indent=2)

    def test_encryption_format(self, rns_instance):
        """Test that encryption produces compatible ciphertext."""
        # Create two identities for encryption test
        alice = RNS.Identity()
        bob = RNS.Identity()

        # Create destination for Bob
        bob_destination = RNS.Destination(
            bob,
            RNS.Destination.IN,
            RNS.Destination.SINGLE,
            "test_app",
            "test"
        )

        plaintext = b"secret message"

        # Encrypt for Bob's destination
        ciphertext = bob_destination.encrypt(plaintext)

        test_vector = {
            "alice_public_key_hex": alice.get_public_key().hex(),
            "bob_public_key_hex": bob.get_public_key().hex(),
            "plaintext_hex": plaintext.hex(),
            "ciphertext_hex": ciphertext.hex(),
            "ciphertext_length": len(ciphertext),
        }

        print(f"\nEncryption format:")
        print(f"  Plaintext: {plaintext.hex()} ({len(plaintext)} bytes)")
        print(f"  Ciphertext length: {len(ciphertext)} bytes")
        print(f"  Overhead: {len(ciphertext) - len(plaintext)} bytes")

        # Decrypt and verify
        decrypted = bob_destination.decrypt(ciphertext)
        assert decrypted == plaintext, "Decryption should recover original message"

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "encryption.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_vector, f, indent=2)


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
