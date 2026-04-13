#[derive(Debug)]
pub enum RnsError {
    /// TX/RX ring is at capacity; no slot available.
    WindowFull,
    /// Output buffer is too small to hold the requested data.
    OutOfMemory,
    /// A function argument was out of range or otherwise invalid.
    InvalidArgument,
    /// Signature verification failed.
    IncorrectSignature,
    /// Hash length or content is incorrect.
    IncorrectHash,
    /// A cryptographic operation failed (key parse, encrypt, decrypt, etc.).
    CryptoError,
    /// A packet could not be constructed or parsed.
    PacketError,
    /// A connection-level error occurred.
    ConnectionError,
}
