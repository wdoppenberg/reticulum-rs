use reticulum_tokio::ChannelError;

#[derive(Debug)]
pub enum ChatError {
    Channel(ChannelError),
    Decode(String),
    NoLink,
    Closed,
    /// Connection attempt timed out before a channel was established.
    Timeout,
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Channel(e) => write!(f, "channel error: {}", e),
            ChatError::Decode(e) => write!(f, "decode error: {}", e),
            ChatError::NoLink => write!(f, "no active channel to peer"),
            ChatError::Closed => write!(f, "chat node is closed"),
            ChatError::Timeout => write!(f, "connection timed out"),
        }
    }
}

impl std::error::Error for ChatError {}

impl From<ChannelError> for ChatError {
    fn from(e: ChannelError) -> Self {
        ChatError::Channel(e)
    }
}
