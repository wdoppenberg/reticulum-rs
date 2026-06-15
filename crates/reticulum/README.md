# Reticulum-rs

**Reticulum-rs** is a Rust implementation of the [Reticulum Network Stack](https://reticulum.network/) — a cryptographic, decentralised, and resilient mesh networking protocol designed for communication over any physical layer.

This project brings Reticulum's capabilities to the Rust ecosystem, enabling embedded and constrained deployments with minimal dependencies.

## Features

- Cryptographic mesh networking (Ed25519 identity, X25519 link DH, Fernet-style AES-CBC + HMAC-SHA256 payloads)
- Trustless routing via identity-based addresses
- `no_std`-compatible core suitable for Cortex-M class targets
- Multiple transport layers: TCP, UDP, serial/HDLC, IPv6 link-local auto-discovery, I2P
- Embassy and Tokio async backends from a single core protocol crate

## Workspace layout

The codebase is a Cargo workspace; pick the crate that matches your target.

| Crate                | Purpose                                                                                  |
|----------------------|------------------------------------------------------------------------------------------|
| `reticulum-core`     | `no_std` protocol types, codec, crypto primitives, identity, link state machine.         |
| `reticulum-tokio`    | Tokio-based transport, interface manager, link manager, channel/request/resource layers. |
| `reticulum-embassy`  | Embassy-based interface infrastructure for bare-metal/RTOS targets.                      |
| `reticulum-node`     | Pure `no_std` router, storage, LoRa driver glue for embedded nodes.                      |
| `reticulum-chat`     | Building blocks for chat-style applications on top of the Tokio transport.               |
| `reticulum-tui`      | Ratatui-based terminal chat client.                                                      |
| `reticulum`          | Convenience facade that re-exports `reticulum-core` and (by default) `reticulum-tokio`.  |

Board-specific firmware lives under `boards/` (e.g. `boards/heltec-t114` for the nRF52840 + SX1262 Heltec T114).

## Getting started

### Prerequisites

- Rust 1.85 or newer (edition 2021)

### Build the host stack

```bash
cargo build -p reticulum-tokio --release
```

### Run the workspace test suite

```bash
just test
# or:
cargo test -p reticulum-core -p reticulum-tokio -p reticulum-chat
```

### Cross-implementation conformance

`tests/compat/` contains Python compatibility tests that exercise the wire codec against upstream Python RNS. See `tests/compat/README.md` for setup.

## Compatibility with upstream Reticulum

This implementation targets wire compatibility with the Python Reticulum Network Stack. The current state of conformance is summarised below; see also `tests/compat/RESULTS.md`.

| Feature                              | Status      | Notes                                                                                  |
|--------------------------------------|-------------|----------------------------------------------------------------------------------------|
| Packet codec (header, ifac, body)    | Implemented | Round-trip and arbitrary-input fuzz tests in `reticulum-core`.                         |
| Identity (Ed25519 + X25519, HKDF)    | Implemented | Byte-for-byte hash/sig compatibility with Python RNS.                                  |
| `Single` destination announces       | Implemented | Validated against Python-generated announces.                                          |
| `Plain` destinations                 | Implemented |                                                                                        |
| `Group` destinations                 | Stub        | `Group` and `GroupIdentity` types exist but are not implemented.                       |
| Path requests / responses            | Implemented |                                                                                        |
| Link establishment + proof           | Implemented | Type-state via `ActiveLink` capability token.                                          |
| Channel (in-link messaging)          | Implemented |                                                                                        |
| Resource transfer                    | Partial     | Codec and segmentation implemented; receiver-side state machine still under iteration. |
| Request/response over links          | Implemented |                                                                                        |
| IFAC (interface authentication code) | Not yet     | `IfacFlag::Authenticated` is parsed but the HMAC is not computed/verified.             |

Wire-format invariants enforced by the codec:

- The 2-bit `propagation_type` field rejects reserved values (`0b10`, `0b11`).
- Unknown `PacketContext` values cause a parse failure rather than silent
  remapping to `None`.
- `Header::try_from_meta` is fallible — every other bit field is validated.

## Use cases

- Resilient messaging over sub-GHz LoRa or other narrowband links
- UAV-to-ground command, control, and telemetry
- Infrastructure-free, identity-authenticated peer messaging

## License

This project is licensed under the MIT license.

---

© Beechat Network Systems Ltd.
https://beechat.network/
