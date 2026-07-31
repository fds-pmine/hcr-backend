//! Sans-io session: bytes in, bytes out.
//!
//! Owns the decoder, answers pings, tracks the closing handshake, and exposes
//! the application payload as a plain byte stream. It performs no IO itself, so
//! it can be driven by hotaru's `HotaruRead`/`HotaruWrite`, by tokio, or by a
//! test harness with no runtime at all.

use alloc::vec::Vec;

use crate::codec::{Decoder, Event};
use crate::error::{CloseFrame, WsError, WsResult};
use crate::frame::{OpCode, encode_server_frame};

/// Normal closure (RFC 6455 §7.4.1).
pub const CLOSE_NORMAL: u16 = 1000;
/// The endpoint is going away.
pub const CLOSE_GOING_AWAY: u16 = 1001;

/// A server-side WebSocket connection after a successful handshake.
#[derive(Debug)]
pub struct WsSession {
    decoder: Decoder,
    /// Bytes to hand to the socket.
    outbound: Vec<u8>,
    /// Decoded application bytes.
    inbound: Vec<u8>,
    close_sent: bool,
    close_received: Option<Option<CloseFrame>>,
}

impl Default for WsSession {
    fn default() -> Self {
        Self::new(Decoder::default())
    }
}

impl WsSession {
    /// Wrap a configured decoder.
    pub fn new(decoder: Decoder) -> Self {
        Self {
            decoder,
            outbound: Vec::new(),
            inbound: Vec::new(),
            close_sent: false,
            close_received: None,
        }
    }

    /// Feed bytes read from the socket.
    ///
    /// Pings are answered automatically and a peer close is echoed, so callers
    /// only ever deal with application data.
    pub fn on_bytes(&mut self, incoming: &[u8]) -> WsResult<()> {
        self.decoder.feed(incoming)?;

        while let Some(event) = self.decoder.poll()? {
            match event {
                Event::Data(mut payload) => self.inbound.append(&mut payload),
                // A pong must echo the ping's application data (RFC 6455 §5.5.3).
                Event::Ping(payload) => {
                    encode_server_frame(OpCode::Pong, &payload, &mut self.outbound)
                }
                // Unsolicited pongs are legal and simply ignored (§5.5.3).
                Event::Pong(_) => {}
                Event::Close(frame) => {
                    self.close_received = Some(frame.clone());
                    if !self.close_sent {
                        // Echo the peer's status code where it gave one.
                        let code = frame.as_ref().map_or(CLOSE_NORMAL, |f| f.code);
                        self.send_close(code, "");
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    /// Take decoded application bytes, leaving the buffer empty.
    pub fn take_inbound(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.inbound)
    }

    /// Application bytes decoded so far, without consuming them.
    pub fn inbound(&self) -> &[u8] {
        &self.inbound
    }

    /// Queue application bytes as a single binary frame.
    ///
    /// Server frames are never masked. Writes after a close are dropped, since
    /// sending data once the closing handshake has begun is a protocol error.
    pub fn write(&mut self, payload: &[u8]) {
        if self.close_sent {
            return;
        }
        encode_server_frame(OpCode::Binary, payload, &mut self.outbound);
    }

    /// Queue a ping.
    pub fn ping(&mut self, payload: &[u8]) {
        if self.close_sent {
            return;
        }
        encode_server_frame(OpCode::Ping, payload, &mut self.outbound);
    }

    /// Begin the closing handshake.
    pub fn send_close(&mut self, code: u16, reason: &str) {
        if self.close_sent {
            return;
        }
        self.close_sent = true;

        let mut payload = Vec::with_capacity(2 + reason.len());
        payload.extend_from_slice(&code.to_be_bytes());
        payload.extend_from_slice(reason.as_bytes());
        encode_server_frame(OpCode::Close, &payload, &mut self.outbound);
    }

    /// Close in response to a protocol failure, using the error's own code.
    pub fn send_close_for(&mut self, error: &WsError) {
        self.send_close(error.close_code(), "");
    }

    /// Take the bytes queued for the socket.
    pub fn take_outbound(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.outbound)
    }

    /// Whether anything is waiting to be written.
    pub fn has_outbound(&self) -> bool {
        !self.outbound.is_empty()
    }

    /// Whether a close frame was received.
    pub fn close_received(&self) -> Option<&Option<CloseFrame>> {
        self.close_received.as_ref()
    }

    /// Whether the closing handshake is finished in both directions, so the
    /// socket can be dropped once `outbound` has drained.
    pub fn is_closed(&self) -> bool {
        self.close_sent && self.close_received.is_some()
    }
}
