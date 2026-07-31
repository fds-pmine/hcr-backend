//! Server-side opening handshake (RFC 6455 §4.2).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha1::{Digest, Sha1};

use crate::error::{WsError, WsResult};

/// The fixed GUID concatenated with the client key (RFC 6455 §1.3).
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Subprotocol the MQTT binding mandates.
pub const MQTT_SUBPROTOCOL: &str = "mqtt";

/// The only WebSocket version this implementation speaks.
pub const WS_VERSION: &str = "13";

/// Fields extracted from a client's upgrade request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeRequest {
    /// Requested path, e.g. `/mqtt`.
    pub path: String,
    /// Raw `Sec-WebSocket-Key`.
    pub key: String,
    /// Subprotocols the client offered.
    pub protocols: Vec<String>,
}

/// Parse an HTTP upgrade request.
///
/// Returns `Ok(None)` if the header block is incomplete — the caller should read
/// more bytes. The returned `usize` is the length of the consumed request.
pub fn parse_request(buf: &[u8]) -> WsResult<Option<(HandshakeRequest, usize)>> {
    let Some(end) = find_header_end(buf) else {
        return Ok(None);
    };

    let text = core::str::from_utf8(&buf[..end])
        .map_err(|_| WsError::BadHandshake("request is not valid UTF-8"))?;

    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(WsError::BadHandshake("missing request line"))?;

    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or(WsError::BadHandshake("missing method"))?;
    let path = parts.next().ok_or(WsError::BadHandshake("missing path"))?;
    if !method.eq_ignore_ascii_case("GET") {
        return Err(WsError::BadHandshake("method must be GET"));
    }

    let mut upgrade = None;
    let mut connection = None;
    let mut key = None;
    let mut version = None;
    let mut protocols = Vec::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(WsError::BadHandshake("malformed header line"));
        };
        let name = name.trim();
        let value = value.trim();

        // Header names are case-insensitive (RFC 7230 §3.2).
        if name.eq_ignore_ascii_case("upgrade") {
            upgrade = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("connection") {
            connection = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("sec-websocket-key") {
            key = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("sec-websocket-version") {
            version = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("sec-websocket-protocol") {
            // May appear multiple times, and each may be a comma-separated list.
            for entry in value.split(',') {
                let entry = entry.trim();
                if !entry.is_empty() {
                    protocols.push(entry.to_string());
                }
            }
        }
    }

    let upgrade = upgrade.ok_or(WsError::BadHandshake("missing Upgrade header"))?;
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err(WsError::BadHandshake("Upgrade must be `websocket`"));
    }

    let connection = connection.ok_or(WsError::BadHandshake("missing Connection header"))?;
    // Connection is a comma-separated list of tokens; `Upgrade` must be one.
    if !connection
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    {
        return Err(WsError::BadHandshake("Connection must include `Upgrade`"));
    }

    let version = version.ok_or(WsError::BadHandshake("missing Sec-WebSocket-Version"))?;
    if version != WS_VERSION {
        return Err(WsError::BadHandshake("Sec-WebSocket-Version must be 13"));
    }

    let key = key.ok_or(WsError::BadHandshake("missing Sec-WebSocket-Key"))?;
    // The key is 16 random bytes, base64-encoded: always 24 characters.
    if key.len() != 24 || BASE64.decode(key.as_bytes()).map(|k| k.len()) != Ok(16) {
        return Err(WsError::BadHandshake(
            "Sec-WebSocket-Key must be 16 base64-encoded bytes",
        ));
    }

    Ok(Some((
        HandshakeRequest {
            path: path.to_string(),
            key,
            protocols,
        },
        end,
    )))
}

/// Compute `Sec-WebSocket-Accept` (RFC 6455 §4.2.2).
///
/// SHA-1 here is a fixed handshake checksum defined by the RFC, not a security
/// mechanism — its cryptographic weakness is irrelevant to this use.
pub fn accept_key(client_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    BASE64.encode(hasher.finalize())
}

impl HandshakeRequest {
    /// Whether the client offered the `mqtt` subprotocol.
    pub fn offers_mqtt(&self) -> bool {
        self.protocols
            .iter()
            .any(|p| p.eq_ignore_ascii_case(MQTT_SUBPROTOCOL))
    }

    /// Build the `101 Switching Protocols` response for an MQTT client.
    ///
    /// # Errors
    /// Returns [`WsError::SubprotocolNotOffered`] if the client did not offer
    /// `mqtt`. The MQTT specification's WebSocket binding requires it, so
    /// accepting without it would leave both ends guessing at the framing.
    pub fn accept_mqtt(&self) -> WsResult<String> {
        if !self.offers_mqtt() {
            return Err(WsError::SubprotocolNotOffered);
        }
        Ok(response(&accept_key(&self.key), Some(MQTT_SUBPROTOCOL)))
    }
}

/// Render a `101 Switching Protocols` response.
pub fn response(accept: &str, subprotocol: Option<&str>) -> String {
    let mut out = String::from("HTTP/1.1 101 Switching Protocols\r\n");
    out.push_str("Upgrade: websocket\r\n");
    out.push_str("Connection: Upgrade\r\n");
    out.push_str(&format!("Sec-WebSocket-Accept: {accept}\r\n"));
    if let Some(protocol) = subprotocol {
        out.push_str(&format!("Sec-WebSocket-Protocol: {protocol}\r\n"));
    }
    out.push_str("\r\n");
    out
}

/// Render a rejection response for a failed handshake.
pub fn reject(error: &WsError) -> String {
    let (status, body) = match error {
        WsError::SubprotocolNotOffered => (
            "400 Bad Request",
            "this endpoint serves the `mqtt` subprotocol only",
        ),
        _ => ("400 Bad Request", "invalid WebSocket handshake"),
    };
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Index just past the terminating `\r\n\r\n`, if the header block is complete.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}
