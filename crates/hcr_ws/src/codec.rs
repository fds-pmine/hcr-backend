//! Incremental frame decoding.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{CloseFrame, WsError, WsResult};
use crate::frame::{FrameHeader, OpCode, apply_mask};

/// Something the peer sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Payload bytes belonging to the application stream.
    ///
    /// For MQTT these are simply appended: WebSocket message boundaries carry no
    /// meaning, so a packet may span frames and a frame may hold several packets.
    Data(Vec<u8>),
    /// A ping; the peer expects a pong carrying the same payload.
    Ping(Vec<u8>),
    /// A pong.
    Pong(Vec<u8>),
    /// The peer is closing.
    Close(Option<CloseFrame>),
}

/// Default cap on a single frame's payload.
///
/// Chosen to match `MqttSafety::max_packet_size`'s 1 MiB default so an
/// unauthenticated peer cannot force a large allocation at this layer either.
pub const DEFAULT_MAX_FRAME_PAYLOAD: usize = 1024 * 1024;

/// Default cap on buffered-but-unparsed bytes.
pub const DEFAULT_MAX_BUFFER: usize = DEFAULT_MAX_FRAME_PAYLOAD + 16 * 1024;

/// Turns a byte stream into [`Event`]s.
#[derive(Debug)]
pub struct Decoder {
    buf: Vec<u8>,
    max_frame_payload: usize,
    max_buffer: usize,
    /// A fragmented data message is in progress.
    in_message: bool,
    closed: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAME_PAYLOAD, DEFAULT_MAX_BUFFER)
    }
}

impl Decoder {
    /// Create a decoder with explicit bounds.
    pub fn new(max_frame_payload: usize, max_buffer: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame_payload,
            max_buffer,
            in_message: false,
            closed: false,
        }
    }

    /// Whether a close frame has been seen.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Buffer bytes read from the socket.
    ///
    /// # Errors
    /// [`WsError::BufferOverflow`] if the peer sends more than `max_buffer`
    /// bytes without producing a parseable frame.
    pub fn feed(&mut self, bytes: &[u8]) -> WsResult<()> {
        if self.buf.len() + bytes.len() > self.max_buffer {
            return Err(WsError::BufferOverflow {
                limit: self.max_buffer,
            });
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Pull the next complete frame, if one has arrived.
    ///
    /// Returns `Ok(None)` when more bytes are needed.
    pub fn poll(&mut self) -> WsResult<Option<Event>> {
        let Some((header, header_len)) = FrameHeader::parse(&self.buf, self.max_frame_payload)?
        else {
            return Ok(None);
        };

        let payload_len = header.payload_len as usize;
        let total = header_len + payload_len;
        if self.buf.len() < total {
            return Ok(None);
        }

        // RFC 6455 §5.1: every client-to-server frame must be masked.
        let Some(mask) = header.mask else {
            return Err(WsError::UnmaskedClientFrame);
        };

        let mut payload = self.buf[header_len..total].to_vec();
        apply_mask(&mut payload, mask, 0);
        self.buf.drain(..total);

        // Control frames may be interleaved inside a fragmented data message, so
        // they must not disturb the fragmentation state.
        match header.opcode {
            OpCode::Text => Err(WsError::TextFrameRejected),

            OpCode::Binary => {
                if self.in_message {
                    return Err(WsError::UnexpectedContinuation);
                }
                self.in_message = !header.fin;
                Ok(Some(Event::Data(payload)))
            }

            OpCode::Continuation => {
                if !self.in_message {
                    return Err(WsError::UnexpectedContinuation);
                }
                if header.fin {
                    self.in_message = false;
                }
                Ok(Some(Event::Data(payload)))
            }

            OpCode::Ping => Ok(Some(Event::Ping(payload))),
            OpCode::Pong => Ok(Some(Event::Pong(payload))),

            OpCode::Close => {
                self.closed = true;
                match payload.len() {
                    0 => Ok(Some(Event::Close(None))),
                    1 => Err(WsError::InvalidCloseFrame),
                    _ => {
                        let code = u16::from_be_bytes([payload[0], payload[1]]);
                        let reason = String::from_utf8_lossy(&payload[2..]).into_owned();
                        Ok(Some(Event::Close(Some(CloseFrame { code, reason }))))
                    }
                }
            }
        }
    }
}
