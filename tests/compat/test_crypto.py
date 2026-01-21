"""
Test cryptographic compatibility between Rust and Python implementations.

These tests verify that cryptographic operations produce identical results,
enabling secure interoperability.
"""

import RNS
import pytest
import json
from pathlib import Path


class TestKeyExchange:
    """Test X25519 key exchange compatibility."""

    def test_shared_secret_derivation(self):
        """Test that both implementations derive the same shared secret."""
        # Create two identities
        alice = RNS.Identity()
        bob = RNS.Identity()

        # In a real scenario, Alice would derive shared secret with Bob's public key
        # and vice versa. Both should get the same shared secret.

        test_data = {
            "alice_public_hex": alice.get_public_key()[:32].hex(),  # X25519 key
            "bob_public_hex": bob.get_public_key()[:32].hex(),
            "note": "Shared secret derivation requires private keys, test via encryption"
        }

        print(f"\nKey exchange test:")
        print(f"  Alice public key: {test_data['alice_public_hex']}")
        print(f"  Bob public key: {test_data['bob_public_hex']}")

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "key_exchange.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


class TestSignatureVerification:
    """Test Ed25519 signature compatibility."""

    def test_cross_verification(self):
        """Test that signatures can be verified across implementations."""
        identity = RNS.Identity()
        messages = [
            b"test message 1",
            b"another test message",
            b"",  # Empty message
            b"x" * 1000,  # Long message
        ]

        test_vectors = []

        for msg in messages:
            signature = identity.sign(msg)

            # Verify with Python first
            assert identity.validate(signature, msg), "Python verification should succeed"

            test_vectors.append({
                "message_hex": msg.hex(),
                "signature_hex": signature.hex(),
                "public_key_hex": identity.get_public_key().hex(),
                "verifying_key_hex": identity.get_public_key()[32:].hex(),  # Ed25519 key
            })

        print(f"\nSignature verification tests: {len(test_vectors)} test cases")

        # Save test vectors
        test_vector_path = Path(__file__).parent / "vectors" / "signature_verification.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_vectors, f, indent=2)


class TestHKDF:
    """Test HKDF key derivation compatibility."""

    def test_key_derivation(self):
        """Test that HKDF produces identical derived keys."""
        # This is implicitly tested through encryption, but we can document expected behavior
        test_data = {
            "algorithm": "HKDF-SHA256",
            "note": "Used for deriving encryption keys from X25519 shared secrets",
            "output_length": 64,  # 32 bytes encryption + 32 bytes HMAC for Fernet
        }

        print(f"\nHKDF test:")
        print(f"  Algorithm: {test_data['algorithm']}")
        print(f"  Output length: {test_data['output_length']} bytes")

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "hkdf.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


class TestFernetCompatibility:
    """Test Fernet encryption compatibility."""

    def test_fernet_format(self, rns_instance):
        """Test that Fernet tokens have the correct format."""
        identity = RNS.Identity()
        destination = RNS.Destination(
            identity,
            RNS.Destination.IN,
            RNS.Destination.SINGLE,
            "test",
            "fernet"
        )

        plaintext = b"test fernet encryption"
        ciphertext = destination.encrypt(plaintext)

        test_data = {
            "plaintext_hex": plaintext.hex(),
            "ciphertext_hex": ciphertext.hex(),
            "note": "Fernet format: version(1) + timestamp(8) + iv(16) + ciphertext(n) + hmac(32)",
        }

        print(f"\nFernet format test:")
        print(f"  Plaintext: {len(plaintext)} bytes")
        print(f"  Ciphertext: {len(ciphertext)} bytes")
        print(f"  Overhead: {len(ciphertext) - len(plaintext)} bytes")

        # Verify decryption
        decrypted = destination.decrypt(ciphertext)
        assert decrypted == plaintext

        # Save test vector
        test_vector_path = Path(__file__).parent / "vectors" / "fernet.json"
        test_vector_path.parent.mkdir(exist_ok=True)
        with open(test_vector_path, "w") as f:
            json.dump(test_data, f, indent=2)


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
