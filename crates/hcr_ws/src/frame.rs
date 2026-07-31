//! Frame header parsing and encoding (RFC 6455 §5.2).
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-------+-+-------------+-------------------------------+
//! |F|R|R|R| opcode|M| Payload len |    Extended payload length    |
//! |I|S|S|S|  (4)  |A|     (7)     |             (16/64)           |
//! |N|V|V|V|       |S|             |   (if payload len==126/127)   |
//! | |1|2|3|       |K|             |                               |
//! +-+-+-+-+-------+-+-------------+ - - - - - - - - - - - - - - - +
//! |     Extended payload length continued, if payload len == 127  |
//! + - - - - - - - - - - - - - - - +-------------------------------+
//! |                               |Masking-key, if MASK set to 1  |
//! +-------------------------------+-------------------------------+
//! ```

use alloc::vec::Vec;

use crate::error::{WsError, WsResult};

/// Frame opcode (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    /// Continues the previous data frame.
    Continuation,
    /// UTF-8 text. Rejected by the MQTT binding.
    Text,
    /// Binary payload — what MQTT uses.
    Binary,
    /// Connection close.
    Close,
    /// Liveness probe.
    Ping,
    /// Reply to a ping.
    Pong,
}

impl OpCode {
    /// Decode the 4-bit opcode field.
    pub fn from_bits(bits: u8) -> WsResult<Self> {
        match bits {
            0x0 => Ok(OpCode::Continuation),
            0x1 => Ok(OpCode::Text),
            0x2 => Ok(OpCode::Binary),
            0x8 => Ok(OpCode::Close),
            0x9 => Ok(OpCode::Ping),
            0xA => Ok(OpCode::Pong),
            other => Err(WsError::UnknownOpcode(other)),
        }
    }

    /// Wire representation.
    pub fn bits(self) -> u8 {
        match self {
            OpCode::Continuation => 0x0,
            OpCode::Text => 0x1,
            OpCode::Binary => 0x2,
            OpCode::Close => 0x8,
            OpCode::Ping => 0x9,
            OpCode::Pong => 0xA,
        }
    }

    /// Control frames may not be fragmented and carry at most 125 bytes.
    pub fn is_control(self) -> bool {
        matches!(self, OpCode::Close | OpCode::Ping | OpCode::Pong)
    }
}

/// A parsed frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Final fragment of a message.
    pub fin: bool,
    /// Opcode.
    pub opcode: OpCode,
    /// Masking key, present iff the MASK bit was set.
    pub mask: Option<[u8; 4]>,
    /// Payload length in bytes.
    pub payload_len: u64,
}

impl FrameHeader {
    /// Bytes this header occupies on the wire.
    pub fn encoded_len(&self) -> usize {
        let len_bytes = match self.payload_len {
            0..=125 => 0,
            126..=65535 => 2,
            _ => 8,
        };
        2 + len_bytes + if self.mask.is_some() { 4 } else { 0 }
    }

    /// Try to parse a header from the front of `buf`.
    ///
    /// Returns `Ok(None)` when more bytes are needed — the caller should keep
    /// buffering rather than treat it as an error.
    pub fn parse(buf: &[u8], max_payload: usize) -> WsResult<Option<(FrameHeader, usize)>> {
        if buf.len() < 2 {
            return Ok(None);
        }

        let first = buf[0];
        let second = buf[1];

        // RSV1-3 must be zero: we negotiate no extensions.
        if first & 0x70 != 0 {
            return Err(WsError::ReservedBitSet);
        }

        let fin = first & 0x80 != 0;
        let opcode = OpCode::from_bits(first & 0x0F)?;
        let masked = second & 0x80 != 0;
        let short_len = second & 0x7F;

        let mut cursor = 2usize;
        let payload_len: u64 = match short_len {
            126 => {
                if buf.len() < cursor + 2 {
                    return Ok(None);
                }
                let len = u16::from_be_bytes([buf[cursor], buf[cursor + 1]]) as u64;
                cursor += 2;
                len
            }
            127 => {
                if buf.len() < cursor + 8 {
                    return Ok(None);
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[cursor..cursor + 8]);
                let len = u64::from_be_bytes(bytes);
                // RFC 6455 §5.2: the most significant bit MUST be 0.
                if len & 0x8000_0000_0000_0000 != 0 {
                    return Err(WsError::InvalidPayloadLength);
                }
                cursor += 8;
                len
            }
            other => u64::from(other),
        };

        if opcode.is_control() {
            if !fin {
                return Err(WsError::InvalidControlFrame("must not be fragmented"));
            }
            if payload_len > 125 {
                return Err(WsError::InvalidControlFrame("payload exceeds 125 bytes"));
            }
        }

        // Check the declared length before allocating anything for it.
        if payload_len > max_payload as u64 {
            return Err(WsError::FrameTooLarge {
                declared: payload_len,
                limit: max_payload,
            });
        }

        let mask = if masked {
            if buf.len() < cursor + 4 {
                return Ok(None);
            }
            let key = [buf[cursor], buf[cursor + 1], buf[cursor + 2], buf[cursor + 3]];
            cursor += 4;
            Some(key)
        } else {
            None
        };

        Ok(Some((
            FrameHeader {
                fin,
                opcode,
                mask,
                payload_len,
            },
            cursor,
        )))
    }

    /// Append this header's wire form to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut first = self.opcode.bits();
        if self.fin {
            first |= 0x80;
        }
        out.push(first);

        let mask_bit = if self.mask.is_some() { 0x80 } else { 0 };
        match self.payload_len {
            len @ 0..=125 => out.push(mask_bit | len as u8),
            len @ 126..=65535 => {
                out.push(mask_bit | 126);
                out.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                out.push(mask_bit | 127);
                out.extend_from_slice(&len.to_be_bytes());
            }
        }

        if let Some(key) = self.mask {
            out.extend_from_slice(&key);
        }
    }
}

/// Apply the XOR mask in place (RFC 6455 §5.3).
///
/// `offset` is how many payload bytes were already masked, so the key phase is
/// preserved when a payload is processed in chunks. Masking is an involution, so
/// this both masks and unmasks.
pub fn apply_mask(data: &mut [u8], key: [u8; 4], offset: usize) {
    for (index, byte) in data.iter_mut().enumerate() {
        *byte ^= key[(offset + index) % 4];
    }
}

/// Encode a complete server-to-client frame.
///
/// Server frames are never masked (RFC 6455 §5.1), so no key is emitted.
pub fn encode_server_frame(opcode: OpCode, payload: &[u8], out: &mut Vec<u8>) {
    FrameHeader {
        fin: true,
        opcode,
        mask: None,
        payload_len: payload.len() as u64,
    }
    .encode(out);
    out.extend_from_slice(payload);
}
