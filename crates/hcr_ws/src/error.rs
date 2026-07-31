//! Protocol failures.

use alloc::string::String;
use core::fmt;

/// Why a WebSocket exchange must be aborted.
///
/// Every variant here is a *protocol* error: the peer sent something RFC 6455 or
/// the MQTT binding forbids. Each maps to a close code via [`WsError::close_code`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsError {
    /// The HTTP upgrade request was not a valid WebSocket handshake.
    BadHandshake(&'static str),
    /// The client did not offer the `mqtt` subprotocol.
    ///
    /// The MQTT specification's WebSocket binding requires it, so a client that
    /// omits it is not speaking the protocol we serve.
    SubprotocolNotOffered,
    /// A reserved bit was set without a negotiated extension (RFC 6455 §5.2).
    ReservedBitSet,
    /// The opcode is not one this implementation handles.
    UnknownOpcode(u8),
    /// A text frame arrived. MQTT-over-WebSocket is binary-only.
    TextFrameRejected,
    /// A client-to-server frame was not masked (RFC 6455 §5.1).
    UnmaskedClientFrame,
    /// A control frame exceeded 125 bytes or was fragmented (RFC 6455 §5.5).
    InvalidControlFrame(&'static str),
    /// A continuation frame arrived with no message in progress, or a new data
    /// frame arrived while one was.
    UnexpectedContinuation,
    /// The peer declared a payload larger than this endpoint accepts.
    FrameTooLarge {
        /// Length the peer declared.
        declared: u64,
        /// Largest length accepted.
        limit: usize,
    },
    /// A 64-bit length had its most significant bit set (RFC 6455 §5.2).
    InvalidPayloadLength,
    /// A close frame carried a 1-byte payload, which cannot hold a status code.
    InvalidCloseFrame,
    /// The accumulated buffer exceeded its bound before yielding a frame.
    BufferOverflow {
        /// Largest buffer accepted.
        limit: usize,
    },
}

impl WsError {
    /// RFC 6455 §7.4.1 close code to report for this failure.
    pub fn close_code(&self) -> u16 {
        match self {
            // 1009 Message Too Big.
            WsError::FrameTooLarge { .. } | WsError::BufferOverflow { .. } => 1009,
            // 1003 Unsupported Data — the frame was well-formed but unacceptable.
            WsError::TextFrameRejected => 1003,
            // 1002 Protocol Error for everything else.
            _ => 1002,
        }
    }

    /// Whether this failure happened before the connection was established.
    pub fn is_handshake(&self) -> bool {
        matches!(
            self,
            WsError::BadHandshake(_) | WsError::SubprotocolNotOffered
        )
    }
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsError::BadHandshake(reason) => write!(f, "invalid WebSocket handshake: {reason}"),
            WsError::SubprotocolNotOffered => {
                write!(f, "client did not offer the `mqtt` subprotocol")
            }
            WsError::ReservedBitSet => write!(f, "reserved bit set without a negotiated extension"),
            WsError::UnknownOpcode(code) => write!(f, "unknown opcode 0x{code:x}"),
            WsError::TextFrameRejected => {
                write!(f, "text frame rejected; MQTT-over-WebSocket is binary-only")
            }
            WsError::UnmaskedClientFrame => write!(f, "client-to-server frame was not masked"),
            WsError::InvalidControlFrame(reason) => write!(f, "invalid control frame: {reason}"),
            WsError::UnexpectedContinuation => write!(f, "unexpected continuation frame"),
            WsError::FrameTooLarge { declared, limit } => {
                write!(f, "frame of {declared} bytes exceeds the {limit}-byte limit")
            }
            WsError::InvalidPayloadLength => write!(f, "64-bit payload length has its MSB set"),
            WsError::InvalidCloseFrame => write!(f, "close frame payload is 1 byte"),
            WsError::BufferOverflow { limit } => {
                write!(f, "receive buffer exceeded {limit} bytes")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for WsError {}

/// Result alias for this crate.
pub type WsResult<T> = Result<T, WsError>;

/// A close frame's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    /// RFC 6455 §7.4 status code.
    pub code: u16,
    /// Optional UTF-8 reason.
    pub reason: String,
}
