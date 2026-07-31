//! RFC 6455 WebSocket framing for the MQTT-over-WebSocket binding.
//!
//! Browsers cannot open raw TCP sockets, so a browser MQTT client must speak
//! MQTT inside WebSocket frames. This crate is the framing layer that makes that
//! possible (`docs/backend/04-SERVICE.md` §3).
//!
//! # Sans-io
//!
//! Nothing here performs IO or touches an async runtime. [`WsSession`] takes
//! bytes and returns bytes, which means:
//!
//! * it is exhaustively testable without sockets or a runtime;
//! * it can be driven by hotaru's `HotaruRead`/`HotaruWrite`, by tokio, or by
//!   anything else, without the framing logic knowing the difference;
//! * it builds on `no_std + alloc`.
//!
//! The hotaru `ConnStream` adapter lives outside this crate deliberately. Because
//! `MqttServerProtocol<W, TS, Rt>` is generic over its stream, wiring MQTT over
//! WebSocket needs *only* a `ConnStream` implementation — no change to
//! `hotaru_mqtt` itself.
//!
//! # What the MQTT binding requires
//!
//! Beyond plain RFC 6455, the MQTT specification's WebSocket binding adds three
//! rules, all enforced here:
//!
//! 1. The subprotocol must be negotiated as `mqtt`
//!    ([`HandshakeRequest::accept_mqtt`]).
//! 2. Payloads travel in **binary** frames; a text frame is a protocol error
//!    ([`WsError::TextFrameRejected`]).
//! 3. Frame boundaries carry no meaning — an MQTT packet may span frames and one
//!    frame may contain several packets. [`WsSession`] therefore presents the
//!    payload as a flat byte stream rather than as discrete messages, which is
//!    exactly the shape a byte-stream transport needs.
//!
//! # Example
//!
//! ```
//! use hcr_ws::{HandshakeRequest, WsSession, parse_request};
//!
//! let request = b"GET /mqtt HTTP/1.1\r\n\
//!                 Host: example.com\r\n\
//!                 Upgrade: websocket\r\n\
//!                 Connection: Upgrade\r\n\
//!                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
//!                 Sec-WebSocket-Protocol: mqtt\r\n\
//!                 Sec-WebSocket-Version: 13\r\n\r\n";
//!
//! let (handshake, _consumed) = parse_request(request).unwrap().unwrap();
//! let response = handshake.accept_mqtt().unwrap();
//! assert!(response.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
//! assert!(response.contains("Sec-WebSocket-Protocol: mqtt"));
//!
//! // After the handshake the session is a byte pipe in both directions.
//! let mut session = WsSession::default();
//! session.write(b"\x10\x0c"); // e.g. the head of an MQTT CONNECT
//! assert!(session.has_outbound());
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod codec;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod session;

pub use codec::{DEFAULT_MAX_BUFFER, DEFAULT_MAX_FRAME_PAYLOAD, Decoder, Event};
pub use error::{CloseFrame, WsError, WsResult};
pub use frame::{FrameHeader, OpCode, apply_mask, encode_server_frame};
pub use handshake::{
    HandshakeRequest, MQTT_SUBPROTOCOL, WS_GUID, WS_VERSION, accept_key, parse_request, reject,
    response,
};
pub use session::{CLOSE_GOING_AWAY, CLOSE_NORMAL, WsSession};
