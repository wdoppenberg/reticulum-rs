# Reticulum-rs Implementation Plan

**Goal**: Create a complete Rust implementation of the Reticulum Network Stack that matches the reference Python implementation, with proper testing, API parity, and documentation. Once stable, extend to embedded hardware (nRF52840 + SX1262).

**Approach**: Focus on `reticulum-core` (no_std) + `reticulum-tokio` (async runtime) first, ensuring feature parity with the reference implementation through comprehensive testing.

---

## Phase 1: Core Protocol Completion (High Priority)

### 1.1 Resource Transfer System ⭐⭐⭐
**Status**: Not implemented
**Reference**: `RNS/Resource.py`
**Priority**: CRITICAL - Required for file transfers and large data transmission

**Tasks**:
- [ ] Study Python Resource implementation thoroughly
- [ ] Design no_std-compatible Resource API
- [ ] Implement Resource segmentation and sequencing
- [ ] Add compression support (optional for embedded)
- [ ] Implement windowed transfer protocol
- [ ] Add transfer progress tracking
- [ ] Implement ResourceAdvertisement packet handling
- [ ] Add resume capability for interrupted transfers
- [ ] Write unit tests for Resource
- [ ] Write integration tests with Link
- [ ] Document Resource API with examples

**Files to create/modify**:
- `crates/reticulum-core/src/resource.rs` (new)
- `crates/reticulum-tokio/src/resource.rs` (new, runtime wrapper)
- Update `packet.rs` for Resource contexts

**Testing requirements**:
- Test small file transfers (< MTU)
- Test large file transfers with segmentation
- Test interrupted transfer resume
- Test transfer progress callbacks
- Test compression on/off

---

### 1.2 Channel System ⭐⭐⭐
**Status**: Not implemented
**Reference**: `RNS/Channel.py`
**Priority**: CRITICAL - Required for reliable bidirectional messaging

**Tasks**:
- [ ] Study Python Channel implementation
- [ ] Design Channel API with message type registration
- [ ] Implement sliding window protocol for reliability
- [ ] Add sequence tracking and ordering
- [ ] Implement automatic retry with exponential backoff
- [ ] Add message envelope handling
- [ ] Implement MessageBase trait system
- [ ] Add RawChannelReader/Writer for streaming
- [ ] Write unit tests for Channel
- [ ] Write integration tests with Link
- [ ] Document Channel API with examples

**Files to create/modify**:
- `crates/reticulum-core/src/channel.rs` (new)
- `crates/reticulum-tokio/src/channel.rs` (new, runtime wrapper)
- Update `link.rs` to support channels

**Testing requirements**:
- Test message ordering
- Test retry mechanism
- Test message types and handlers
- Test window size adaptation
- Test channel over unreliable link

---

### 1.3 Request/Response System ⭐⭐
**Status**: Not implemented
**Reference**: `RNS/Request.py`, `RNS/Response.py`
**Priority**: HIGH - RPC-like pattern used throughout ecosystem

**Tasks**:
- [ ] Study Python Request/Response implementation
- [ ] Design Request/Response API
- [ ] Implement request tracking and timeout
- [ ] Add response routing back to requester
- [ ] Implement request handlers on destinations
- [ ] Add support for request parameters
- [ ] Write unit tests
- [ ] Write integration tests
- [ ] Document with examples

**Files to create/modify**:
- `crates/reticulum-core/src/request.rs` (new)
- Update `destination.rs` for request handlers
- Update `packet.rs` for Request/Response contexts

**Testing requirements**:
- Test request/response round-trip
- Test timeout handling
- Test concurrent requests
- Test request parameters

---

## Phase 2: Main Stack & Configuration (High Priority)

### 2.1 Main Reticulum Class ⭐⭐⭐
**Status**: Not implemented
**Reference**: `RNS/Reticulum.py`
**Priority**: CRITICAL - Top-level API for stack initialization

**Tasks**:
- [ ] Study Python Reticulum class
- [ ] Design configuration structure (TOML or similar)
- [ ] Implement configuration file parsing
- [ ] Add interface discovery and management
- [ ] Implement shared instance support (local IPC)
- [ ] Add MTU discovery
- [ ] Implement caching and persistence layer
- [ ] Add logging configuration
- [ ] Write unit tests
- [ ] Write integration tests
- [ ] Document with configuration examples

**Files to create/modify**:
- `crates/reticulum-tokio/src/reticulum.rs` (new)
- `crates/reticulum-tokio/src/config.rs` (new)
- Create example config files

**Testing requirements**:
- Test config file parsing
- Test interface initialization from config
- Test multiple interfaces simultaneously
- Test config validation

---

### 2.2 Discovery & Resolver ⭐⭐
**Status**: Partially implemented (announces work)
**Reference**: `RNS/Discovery.py`, `RNS/Resolver.py`
**Priority**: MEDIUM-HIGH

**Tasks**:
- [ ] Study Python Discovery/Resolver implementation
- [ ] Implement InterfaceAnnouncer for local discovery
- [ ] Add path caching and resolution
- [ ] Implement destination name resolution
- [ ] Add announce rate limiting (already have basic version)
- [ ] Write unit tests
- [ ] Write integration tests
- [ ] Document Discovery API

**Files to create/modify**:
- `crates/reticulum-tokio/src/discovery.rs` (new)
- `crates/reticulum-tokio/src/resolver.rs` (new)
- Enhance existing announce_table logic

**Testing requirements**:
- Test local network discovery
- Test path resolution
- Test announce rate limiting

---

## Phase 3: Interface Expansion (Medium Priority)

### 3.1 AutoInterface ⭐⭐
**Status**: Not implemented
**Reference**: `RNS/Interfaces/AutoInterface.py`
**Priority**: MEDIUM - Important for ease of use

**Tasks**:
- [ ] Study Python AutoInterface implementation
- [ ] Implement network interface discovery (platform-specific)
- [ ] Add multicast group management
- [ ] Implement peer discovery on local networks
- [ ] Add IPv4/IPv6 support
- [ ] Write unit tests
- [ ] Write integration tests
- [ ] Document AutoInterface configuration

**Files to create/modify**:
- `crates/reticulum-tokio/src/iface/auto.rs` (new)
- Platform-specific code for network discovery

**Testing requirements**:
- Test discovery on local networks
- Test multicast operation
- Test IPv4 and IPv6

---

### 3.2 Serial Interface ⭐⭐⭐
**Status**: Not implemented
**Reference**: `RNS/Interfaces/SerialInterface.py`
**Priority**: HIGH - Critical path to embedded

**Tasks**:
- [ ] Study Python SerialInterface implementation
- [ ] Design async serial interface using tokio-serial
- [ ] Add baud rate configuration
- [ ] Implement flow control
- [ ] Add framing (HDLC already exists, may reuse)
- [ ] Write unit tests (with pty emulation)
- [ ] Write integration tests
- [ ] Document Serial configuration

**Files to create/modify**:
- `crates/reticulum-tokio/src/iface/serial.rs` (new)

**Testing requirements**:
- Test with virtual serial ports
- Test different baud rates
- Test flow control
- Test error recovery

---

### 3.3 I2P Interface ⭐
**Status**: Not implemented
**Reference**: `RNS/Interfaces/I2PInterface.py`
**Priority**: LOW - Nice to have for anonymity

**Tasks**:
- [ ] Study Python I2PInterface
- [ ] Research Rust I2P libraries
- [ ] Implement I2P tunnel management
- [ ] Add persistent I2P address handling
- [ ] Write tests
- [ ] Document

**Files to create/modify**:
- `crates/reticulum-tokio/src/iface/i2p.rs` (new)

---

## Phase 4: Testing & Documentation (Continuous)

### 4.1 Comprehensive Test Suite ⭐⭐⭐
**Status**: Basic tests exist
**Priority**: CRITICAL - Ensures correctness

**Tasks**:
- [ ] Create test utilities for mock interfaces
- [ ] Add property-based testing with proptest
- [ ] Implement fuzzing for packet parsing
- [ ] Create integration test scenarios:
  - [ ] Two-node communication
  - [ ] Multi-hop routing
  - [ ] Link establishment over various interfaces
  - [ ] Resource transfers
  - [ ] Channel messaging
  - [ ] Network partition handling
- [ ] Add benchmark suite for performance tracking
- [ ] Set up CI/CD for automated testing

**Files to create/modify**:
- `crates/reticulum-core/tests/` (expand)
- `crates/reticulum-tokio/tests/` (expand)
- `.github/workflows/` (CI configuration)

---

### 4.2 API Documentation ⭐⭐⭐
**Status**: Minimal
**Priority**: HIGH - Required for adoption

**Tasks**:
- [ ] Write comprehensive API docs for all public types
- [ ] Create "Getting Started" guide
- [ ] Add usage examples for common scenarios:
  - [ ] Simple message exchange
  - [ ] File transfer
  - [ ] Establishing links
  - [ ] Creating destinations
  - [ ] Path discovery
- [ ] Document configuration format
- [ ] Create architecture documentation
- [ ] Add protocol specification documentation
- [ ] Generate docs with `cargo doc`

**Files to create/modify**:
- Add doc comments throughout codebase
- `docs/` directory (new)
- `README.md` improvements

---

### 4.3 Compatibility Testing ⭐⭐⭐
**Status**: Not started
**Priority**: CRITICAL - Must interoperate with Python implementation

**Tasks**:
- [ ] Set up test harness with Python Reticulum
- [ ] Test packet format compatibility
- [ ] Test link establishment Rust <-> Python
- [ ] Test announce propagation
- [ ] Test file transfers between implementations
- [ ] Test multi-hop routing in mixed network
- [ ] Document any deviations from reference implementation

**Files to create/modify**:
- `tests/compat/` (new directory)
- Python test scripts for compatibility

---

## Phase 5: Performance & Optimization (Medium Priority)

### 5.1 Memory Optimization ⭐⭐
**Status**: Basic no_std support exists
**Priority**: MEDIUM - Important for embedded future

**Tasks**:
- [ ] Profile memory usage patterns
- [ ] Optimize buffer sizes and allocations
- [ ] Add compile-time size configuration
- [ ] Implement zero-copy where possible
- [ ] Add memory usage documentation

---

### 5.2 Async Runtime Optimization ⭐
**Status**: Basic tokio implementation
**Priority**: MEDIUM

**Tasks**:
- [ ] Profile async task overhead
- [ ] Optimize channel usage between tasks
- [ ] Reduce lock contention
- [ ] Add configurable buffer sizes
- [ ] Benchmark against Python implementation

---

## Phase 6: Embedded Preparation (Future)

### 6.1 Embassy Runtime Support ⭐⭐⭐
**Status**: Not started
**Priority**: HIGH (for embedded goal)

**Tasks**:
- [ ] Create `reticulum-embassy` crate
- [ ] Port interfaces to embassy-rs async
- [ ] Implement embassy-compatible timers
- [ ] Add embedded-friendly logging
- [ ] Test on actual hardware

---

### 6.2 LoRa/Radio Support ⭐⭐⭐
**Status**: Not started
**Priority**: HIGH (for embedded goal)

**Tasks**:
- [ ] Create `reticulum-lora` crate
- [ ] Integrate SX1262 driver
- [ ] Implement RNode protocol
- [ ] Add radio parameter configuration
- [ ] Test on nRF52840 + SX1262

---

## Current Milestone Goals

### Milestone 1: Core Protocol Complete
**Target**: Resource + Channel + Request/Response working
**Success criteria**:
- Can transfer files between Rust nodes
- Can send reliable messages via Channel
- Can do RPC-style communication
- All features tested and documented

### Milestone 2: Full Python Compatibility
**Target**: Rust <-> Python interoperability
**Success criteria**:
- Rust node can join Python network
- Can exchange files with Python nodes
- Can establish links between implementations
- Pass all compatibility tests

### Milestone 3: Production Ready
**Target**: Complete, documented, tested implementation
**Success criteria**:
- All interfaces implemented
- Comprehensive documentation
- Full test coverage
- Performance benchmarks
- Ready for embedded work

---

## Notes & Decisions

### Architecture Decisions
- Keep `reticulum-core` pure no_std for embedded compatibility ✅
- Use `reticulum-tokio` for async runtime with stdlib ✅
- Future: `reticulum-embassy` for embedded async
- Future: `reticulum-lora` for radio drivers

### Testing Strategy
- Unit tests in each crate
- Integration tests in workspace
- Compatibility tests with Python
- Property-based testing for correctness
- Fuzzing for security

### Documentation Strategy
- Inline rustdoc for API
- Separate guides for usage
- Examples for common patterns
- Architecture documentation
- Protocol specification

---

## Progress Tracking

**Last updated**: 2025-01-21

**Current focus**: Creating implementation plan

**Completed**:
- [x] Core identity and cryptography
- [x] Basic packet structure
- [x] Link establishment and management
- [x] Basic transport routing
- [x] Path discovery
- [x] Announce handling
- [x] Basic interfaces (TCP, UDP, HDLC)

**In progress**:
- [ ] Planning and prioritization

**Blocked**:
- None currently
