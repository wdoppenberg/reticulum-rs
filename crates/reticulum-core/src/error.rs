use thiserror::Error;

#[derive(Debug, Error, Copy, Clone, Eq, PartialEq)]
pub enum RnsError {
    /// TX/RX ring is at capacity; no slot available.
    #[error("window is full")]
    WindowFull,
    /// Output buffer is too small to hold the requested data.
    #[error("output buffer is too small")]
    OutOfMemory,
    /// A function argument was out of range or otherwise invalid.
    #[error("invalid argument")]
    InvalidArgument,
    /// Signature verification failed.
    #[error("signature verification failed")]
    IncorrectSignature,
    /// Hash length or content is incorrect.
    #[error("incorrect hash")]
    IncorrectHash,
    /// A cryptographic operation failed (key parse, encrypt, decrypt, etc.).
    #[error("cryptographic operation failed")]
    CryptoError,
    /// The random number generator failed to provide entropy.
    #[error("randomness source failed")]
    Randomness,
    /// A packet could not be constructed or parsed.
    #[error("packet error")]
    PacketError,
    /// A connection-level error occurred.
    #[error("connection error")]
    ConnectionError,
}
