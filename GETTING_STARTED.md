# Getting Started with Reticulum-rs Development

This guide helps you start contributing to or using reticulum-rs.

---

## Quick Start for Development

### Prerequisites
```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone the repository
git clone https://github.com/YOUR_USERNAME/reticulum-rs
cd reticulum-rs
```

### Building
```bash
# Build all crates
cargo build --workspace

# Build with release optimizations
cargo build --workspace --release

# Build only core (no_std)
cargo build -p reticulum-core --no-default-features
```

### Testing
```bash
# Run all tests
cargo test --workspace

# Run tests with output
cargo test --workspace -- --nocapture

# Run specific test
cargo test -p reticulum-tokio hop_test

# Run tests with logging
RUST_LOG=trace cargo test --workspace -- --nocapture
```

### Running Examples
```bash
# List available examples
ls examples/

# Run an example
cargo run --example tcp_server
cargo run --example tcp_client
```

---

## Project Structure

```
reticulum-rs/
├── crates/
│   ├── reticulum-core/     # No-std protocol implementation
│   │   ├── src/
│   │   │   ├── identity.rs     # Cryptographic identities
│   │   │   ├── packet.rs       # Packet structure
│   │   │   ├── destination.rs  # Network destinations
│   │   │   ├── link.rs         # Link types (core)
│   │   │   ├── hash.rs         # Hashing utilities
│   │   │   ├── crypt/          # Encryption (Fernet)
│   │   │   └── buffer.rs       # Static buffers
│   │   └── Cargo.toml
│   │
│   ├── reticulum-tokio/    # Tokio async runtime
│   │   ├── src/
│   │   │   ├── transport.rs    # Routing & path discovery
│   │   │   ├── link/           # Link management
│   │   │   ├── iface/          # Network interfaces
│   │   │   └── ...
│   │   ├── tests/          # Integration tests
│   │   └── Cargo.toml
│   │
│   ├── reticulum-kaonic/   # Kaonic protocol (optional)
│   └── reticulum/          # High-level API (future)
│
├── examples/               # Usage examples
├── TODO.md                # Implementation roadmap
├── ANALYSIS.md            # Current state analysis
└── README.md              # Project overview
```

---

## Development Workflow

### 1. Pick a Task
See `TODO.md` for prioritized tasks. Start with:
- Documentation improvements
- Test coverage expansion
- Bug fixes
- Small feature additions

### 2. Create a Branch
```bash
git checkout -b feature/your-feature-name
```

### 3. Write Tests First (TDD)
```rust
#[test]
fn test_your_feature() {
    // Arrange
    let input = setup_test_data();

    // Act
    let result = your_feature(input);

    // Assert
    assert_eq!(result, expected);
}
```

### 4. Implement Feature
- Follow Rust conventions
- Add doc comments
- Handle errors properly
- Keep no_std compatibility in core

### 5. Run Tests & Lints
```bash
# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace -- -D warnings

# Run tests
cargo test --workspace

# Check no_std compatibility
cargo check -p reticulum-core --no-default-features
```

### 6. Commit & Push
```bash
git add .
git commit -m "feat: add your feature description"
git push origin feature/your-feature-name
```

---

## Key Concepts

### Identity
Cryptographic identity with X25519 (encryption) + Ed25519 (signing) keypairs.

```rust
use reticulum_core::identity::PrivateIdentity;
use rand_core::OsRng;

let identity = PrivateIdentity::new_from_rand(OsRng);
let public_id = identity.as_identity();
```

### Destination
Network endpoint that can send/receive packets.

```rust
use reticulum_core::destination::{DestinationName, SingleInputDestination};

let name = DestinationName::new("myapp", "service");
let destination = SingleInputDestination::new(identity, name);
```

### Transport
Handles routing, path discovery, and packet forwarding.

```rust
use reticulum_tokio::{Transport, TransportConfig};

let config = TransportConfig::new("node1", &identity, true);
let transport = Transport::new(config);
```

### Link
Encrypted connection between two destinations.

```rust
let link = transport.link(destination.desc).await;
```

### Packet
Basic unit of network transmission.

```rust
use reticulum_core::packet::Packet;

let packet = Packet::default();
transport.send_packet(packet).await;
```

---

## Common Tasks

### Adding a New Interface Type

1. Create interface file: `crates/reticulum-tokio/src/iface/my_interface.rs`
2. Implement the interface:
```rust
pub struct MyInterface {
    // configuration
}

impl MyInterface {
    pub async fn spawn(context: InterfaceContext<Self>) {
        // implementation
    }
}

impl Interface for MyInterface {
    fn mtu() -> usize { 1500 }
}
```
3. Register in `crates/reticulum-tokio/src/iface.rs`
4. Add tests
5. Add example

### Adding a New Packet Context

1. Add to enum in `crates/reticulum-core/src/packet.rs`:
```rust
pub enum PacketContext {
    // existing...
    MyNewContext = 0x10,
}
```
2. Add to `From<u8>` impl
3. Handle in transport layer if needed

### Adding Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // test
    }

    #[tokio::test]  // For async tests
    async fn test_async_something() {
        // async test
    }
}
```

---

## Debugging Tips

### Enable Logging
```bash
# Trace level (very verbose)
RUST_LOG=trace cargo test -- --nocapture

# Debug level
RUST_LOG=debug cargo run --example tcp_server

# Specific module
RUST_LOG=reticulum_tokio::transport=trace cargo test
```

### Inspect Packets
```rust
println!("Packet: {}", packet);  // Uses Display impl
println!("Hash: {}", packet.hash());
```

### Debug Transport State
```rust
// In tests, access internal state
let handler = transport.get_handler();
let handler = handler.lock().await;
// inspect handler state
```

---

## Resources

### Reticulum Reference Implementation
- **GitHub**: https://github.com/markqvist/Reticulum
- **Manual**: https://reticulum.network/manual/
- **API Reference**: https://markqvist.github.io/Reticulum/manual/reference.html

### Rust Resources
- **Rust Book**: https://doc.rust-lang.org/book/
- **Async Book**: https://rust-lang.github.io/async-book/
- **Tokio Tutorial**: https://tokio.rs/tokio/tutorial

### Cryptography
- **X25519**: Key agreement (ECDH on Curve25519)
- **Ed25519**: Digital signatures
- **Fernet**: Symmetric authenticated encryption

---

## Getting Help

1. **Check Documentation**: Look at rustdoc comments and examples
2. **Read Tests**: Tests show how to use APIs
3. **Reference Implementation**: Compare with Python code
4. **TODO.md**: See planned work and priorities
5. **ANALYSIS.md**: Understand current state

---

## Next Steps

1. **Read TODO.md** to understand priorities
2. **Run tests** to verify your environment
3. **Try examples** to see the system in action
4. **Pick a task** and start contributing!

Good luck! 🦀
