//! RFC 6455 conformance.
//!
//! Wherever the RFC supplies a worked example (§1.3 handshake, §5.7 frames) the
//! test uses those exact bytes rather than values invented here.

use hcr_ws::*;

/// Build a client-to-server frame: always masked, per RFC 6455 §5.1.
fn client_frame(opcode: u8, fin: bool, payload: &[u8], key: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(if fin { 0x80 } else { 0x00 } | opcode);

    let len = payload.len();
    if len <= 125 {
        out.push(0x80 | len as u8);
    } else if len <= 65535 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }

    out.extend_from_slice(&key);
    let mut masked = payload.to_vec();
    apply_mask(&mut masked, key, 0);
    out.extend_from_slice(&masked);
    out
}

const BINARY: u8 = 0x2;
const TEXT: u8 = 0x1;
const CONTINUATION: u8 = 0x0;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

const KEY: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

fn decode_all(bytes: &[u8]) -> WsResult<Vec<Event>> {
    let mut decoder = Decoder::default();
    decoder.feed(bytes)?;
    let mut events = Vec::new();
    while let Some(event) = decoder.poll()? {
        events.push(event);
    }
    Ok(events)
}

// ---------------------------------------------------------------------------
// Handshake — RFC 6455 §1.3 / §4.2
// ---------------------------------------------------------------------------

fn upgrade_request(extra: &str) -> Vec<u8> {
    format!(
        "GET /mqtt HTTP/1.1\r\n\
         Host: example.com\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\
         {extra}\r\n"
    )
    .into_bytes()
}

#[test]
fn accept_key_matches_the_rfc_example() {
    // RFC 6455 §1.3: key "dGhlIHNhbXBsZSBub25jZQ==" yields this accept value.
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[test]
fn valid_mqtt_upgrade_is_accepted() {
    let bytes = upgrade_request("Sec-WebSocket-Protocol: mqtt\r\n");
    let (request, consumed) = parse_request(&bytes).unwrap().unwrap();

    assert_eq!(consumed, bytes.len());
    assert_eq!(request.path, "/mqtt");
    assert!(request.offers_mqtt());

    let response = request.accept_mqtt().unwrap();
    assert!(response.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
    assert!(response.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
    assert!(response.contains("Sec-WebSocket-Protocol: mqtt\r\n"));
    assert!(response.ends_with("\r\n\r\n"));
}

#[test]
fn subprotocol_list_is_split_and_matched_case_insensitively() {
    let bytes = upgrade_request("Sec-WebSocket-Protocol: soap, MQTT , wamp\r\n");
    let (request, _) = parse_request(&bytes).unwrap().unwrap();
    assert!(request.offers_mqtt());
    assert!(request.accept_mqtt().is_ok());
}

#[test]
fn upgrade_without_mqtt_subprotocol_is_refused() {
    let bytes = upgrade_request("Sec-WebSocket-Protocol: chat\r\n");
    let (request, _) = parse_request(&bytes).unwrap().unwrap();
    assert_eq!(
        request.accept_mqtt().unwrap_err(),
        WsError::SubprotocolNotOffered
    );

    // Omitting the header entirely is the same refusal.
    let bare = upgrade_request("");
    let (request, _) = parse_request(&bare).unwrap().unwrap();
    assert_eq!(
        request.accept_mqtt().unwrap_err(),
        WsError::SubprotocolNotOffered
    );
}

#[test]
fn incomplete_header_block_asks_for_more_bytes() {
    let bytes = upgrade_request("Sec-WebSocket-Protocol: mqtt\r\n");
    // Everything except the final blank line.
    let partial = &bytes[..bytes.len() - 2];
    assert_eq!(parse_request(partial).unwrap(), None);
}

#[test]
fn malformed_handshakes_are_rejected() {
    let cases: &[(&str, &str)] = &[
        ("method must be GET", "POST /mqtt HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"),
        ("Upgrade must be `websocket`", "GET /mqtt HTTP/1.1\r\nHost: x\r\nUpgrade: h2c\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"),
        ("Connection must include `Upgrade`", "GET /mqtt HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: keep-alive\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"),
        ("Sec-WebSocket-Version must be 13", "GET /mqtt HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n"),
        ("Sec-WebSocket-Key must be 16 base64-encoded bytes", "GET /mqtt HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: tooshort\r\nSec-WebSocket-Version: 13\r\n\r\n"),
    ];

    for (expected, raw) in cases {
        match parse_request(raw.as_bytes()) {
            Err(WsError::BadHandshake(reason)) => assert_eq!(&reason, expected),
            other => panic!("expected BadHandshake({expected:?}), got {other:?}"),
        }
    }
}

#[test]
fn connection_header_token_list_is_honoured() {
    // Browsers commonly send `Connection: keep-alive, Upgrade`.
    let raw = "GET /mqtt HTTP/1.1\r\nHost: x\r\nUpgrade: WebSocket\r\n\
               Connection: keep-alive, Upgrade\r\n\
               Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
               Sec-WebSocket-Protocol: mqtt\r\nSec-WebSocket-Version: 13\r\n\r\n";
    let (request, _) = parse_request(raw.as_bytes()).unwrap().unwrap();
    assert!(request.accept_mqtt().is_ok());
}

// ---------------------------------------------------------------------------
// Framing — RFC 6455 §5.2 / §5.7
// ---------------------------------------------------------------------------

#[test]
fn rfc_masked_hello_unmasks_correctly() {
    // RFC 6455 §5.7: masked "Hello" is 0x37fa213d keyed to 7f9f4d5158.
    // The RFC uses a text frame; MQTT is binary-only, so the same masking is
    // exercised with opcode 0x2 and the RFC's exact key and ciphertext checked.
    let framed = client_frame(BINARY, true, b"Hello", KEY);
    assert_eq!(
        &framed[6..],
        &[0x7f, 0x9f, 0x4d, 0x51, 0x58],
        "masking must match the RFC 6455 §5.7 worked example"
    );

    let events = decode_all(&framed).unwrap();
    assert_eq!(events, vec![Event::Data(b"Hello".to_vec())]);
}

#[test]
fn masking_is_an_involution() {
    let mut data = *b"Hello, world";
    let original = data;
    apply_mask(&mut data, KEY, 0);
    assert_ne!(data, original);
    apply_mask(&mut data, KEY, 0);
    assert_eq!(data, original);
}

#[test]
fn text_frames_are_rejected() {
    // RFC 6455 §5.7's masked text "Hello" — well-formed, but the MQTT binding
    // forbids text.
    let framed = client_frame(TEXT, true, b"Hello", KEY);
    assert_eq!(decode_all(&framed).unwrap_err(), WsError::TextFrameRejected);
    assert_eq!(WsError::TextFrameRejected.close_code(), 1003);
}

#[test]
fn unmasked_client_frames_are_rejected() {
    // RFC 6455 §5.7's *unmasked* "Hello": legal server-to-client, illegal here.
    let unmasked = [0x82u8, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
    assert_eq!(
        decode_all(&unmasked).unwrap_err(),
        WsError::UnmaskedClientFrame
    );
}

#[test]
fn reserved_bits_are_rejected() {
    let mut framed = client_frame(BINARY, true, b"x", KEY);
    framed[0] |= 0x40; // RSV1
    assert_eq!(decode_all(&framed).unwrap_err(), WsError::ReservedBitSet);
}

#[test]
fn unknown_opcodes_are_rejected() {
    let framed = client_frame(0x3, true, b"x", KEY);
    assert_eq!(decode_all(&framed).unwrap_err(), WsError::UnknownOpcode(0x3));
}

#[test]
fn extended_lengths_round_trip() {
    // 126 selects a 16-bit length; 127 selects 64-bit. RFC 6455 §5.7 uses 256
    // and 65536 byte binary frames for exactly these two cases.
    for size in [125usize, 126, 256, 65_535, 65_536] {
        let payload = vec![0xABu8; size];
        let framed = client_frame(BINARY, true, &payload, KEY);
        let events = decode_all(&framed).unwrap();
        assert_eq!(events, vec![Event::Data(payload)], "size {size} failed");
    }
}

#[test]
fn sixty_four_bit_length_with_msb_set_is_rejected() {
    // RFC 6455 §5.2: the most significant bit of a 64-bit length MUST be 0.
    let mut framed = vec![0x82u8, 0x80 | 127];
    framed.extend_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_be_bytes());
    framed.extend_from_slice(&KEY);
    assert_eq!(
        decode_all(&framed).unwrap_err(),
        WsError::InvalidPayloadLength
    );
}

#[test]
fn oversized_frames_are_rejected_from_the_header_alone() {
    // The declared length is checked before any payload is buffered, so a peer
    // cannot force a large allocation by announcing one.
    let mut decoder = Decoder::new(1024, 64 * 1024);
    let mut header = vec![0x82u8, 0x80 | 127];
    header.extend_from_slice(&(1024u64 * 1024).to_be_bytes());
    header.extend_from_slice(&KEY);

    decoder.feed(&header).unwrap();
    assert_eq!(
        decoder.poll().unwrap_err(),
        WsError::FrameTooLarge {
            declared: 1024 * 1024,
            limit: 1024,
        }
    );
    assert_eq!(
        WsError::FrameTooLarge {
            declared: 1,
            limit: 0
        }
        .close_code(),
        1009
    );
}

#[test]
fn buffer_growth_is_bounded() {
    let mut decoder = Decoder::new(1024, 32);
    assert_eq!(
        decoder.feed(&[0u8; 64]).unwrap_err(),
        WsError::BufferOverflow { limit: 32 }
    );
}

// ---------------------------------------------------------------------------
// Fragmentation — RFC 6455 §5.4
// ---------------------------------------------------------------------------

#[test]
fn fragmented_message_reassembles_in_order() {
    let mut bytes = client_frame(BINARY, false, b"Hel", KEY);
    bytes.extend(client_frame(CONTINUATION, true, b"lo", KEY));

    let events = decode_all(&bytes).unwrap();
    assert_eq!(
        events,
        vec![
            Event::Data(b"Hel".to_vec()),
            Event::Data(b"lo".to_vec())
        ]
    );
}

#[test]
fn continuation_without_a_started_message_is_rejected() {
    let bytes = client_frame(CONTINUATION, true, b"orphan", KEY);
    assert_eq!(
        decode_all(&bytes).unwrap_err(),
        WsError::UnexpectedContinuation
    );
}

#[test]
fn a_new_data_frame_during_fragmentation_is_rejected() {
    let mut bytes = client_frame(BINARY, false, b"Hel", KEY);
    bytes.extend(client_frame(BINARY, true, b"lo", KEY));
    assert_eq!(
        decode_all(&bytes).unwrap_err(),
        WsError::UnexpectedContinuation
    );
}

#[test]
fn control_frames_may_interleave_with_a_fragmented_message() {
    // RFC 6455 §5.4 explicitly permits this, and it must not disturb the
    // fragmentation state.
    let mut bytes = client_frame(BINARY, false, b"Hel", KEY);
    bytes.extend(client_frame(PING, true, b"ping", KEY));
    bytes.extend(client_frame(CONTINUATION, true, b"lo", KEY));

    let events = decode_all(&bytes).unwrap();
    assert_eq!(
        events,
        vec![
            Event::Data(b"Hel".to_vec()),
            Event::Ping(b"ping".to_vec()),
            Event::Data(b"lo".to_vec()),
        ]
    );
}

#[test]
fn control_frames_must_not_be_fragmented_or_oversized() {
    let fragmented = client_frame(PING, false, b"x", KEY);
    assert!(matches!(
        decode_all(&fragmented).unwrap_err(),
        WsError::InvalidControlFrame(_)
    ));

    let oversized = client_frame(PING, true, &[0u8; 126], KEY);
    assert!(matches!(
        decode_all(&oversized).unwrap_err(),
        WsError::InvalidControlFrame(_)
    ));
}

// ---------------------------------------------------------------------------
// The MQTT binding's defining property: frames are not messages
// ---------------------------------------------------------------------------

#[test]
fn one_mqtt_packet_may_span_several_frames() {
    // A CONNECT split mid-packet across two unrelated frames must arrive intact.
    let packet = b"\x10\x0c\x00\x04MQTT\x04\x02\x00\x3c";
    let mut bytes = client_frame(BINARY, true, &packet[..5], KEY);
    bytes.extend(client_frame(BINARY, true, &packet[5..], KEY));

    let mut session = WsSession::default();
    session.on_bytes(&bytes).unwrap();
    assert_eq!(session.take_inbound(), packet.to_vec());
}

#[test]
fn one_frame_may_carry_several_mqtt_packets() {
    let pingreq = b"\xc0\x00";
    let mut payload = Vec::new();
    payload.extend_from_slice(pingreq);
    payload.extend_from_slice(pingreq);
    payload.extend_from_slice(pingreq);

    let bytes = client_frame(BINARY, true, &payload, KEY);
    let mut session = WsSession::default();
    session.on_bytes(&bytes).unwrap();
    assert_eq!(session.take_inbound(), payload);
}

#[test]
fn a_stream_delivered_one_byte_at_a_time_decodes_identically() {
    // Proves the decoder is genuinely incremental: TCP will split anywhere.
    let packet = b"\x10\x0c\x00\x04MQTT\x04\x02\x00\x3c";
    let bytes = client_frame(BINARY, true, packet, KEY);

    let mut session = WsSession::default();
    for byte in &bytes {
        session.on_bytes(&[*byte]).unwrap();
    }
    assert_eq!(session.take_inbound(), packet.to_vec());
}

// ---------------------------------------------------------------------------
// Session behaviour
// ---------------------------------------------------------------------------

#[test]
fn ping_is_answered_with_a_pong_echoing_the_payload() {
    let bytes = client_frame(PING, true, b"heartbeat", KEY);
    let mut session = WsSession::default();
    session.on_bytes(&bytes).unwrap();

    let out = session.take_outbound();
    // Server frames are unmasked (RFC 6455 §5.1): 0x8A, then a 9-byte length.
    assert_eq!(out[0], 0x80 | PONG);
    assert_eq!(out[1], 9, "server frames must not set the MASK bit");
    assert_eq!(&out[2..], b"heartbeat");
}

#[test]
fn unsolicited_pongs_are_ignored() {
    let bytes = client_frame(PONG, true, b"whatever", KEY);
    let mut session = WsSession::default();
    session.on_bytes(&bytes).unwrap();
    assert!(session.inbound().is_empty());
    assert!(!session.has_outbound());
}

#[test]
fn server_writes_are_unmasked_binary_frames() {
    let mut session = WsSession::default();
    session.write(b"\x10\x0c");

    let out = session.take_outbound();
    assert_eq!(out[0], 0x80 | BINARY);
    assert_eq!(out[1], 2);
    assert_eq!(&out[2..], b"\x10\x0c");
}

#[test]
fn peer_close_is_echoed_and_completes_the_handshake() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1000u16.to_be_bytes());
    payload.extend_from_slice(b"bye");
    let bytes = client_frame(CLOSE, true, &payload, KEY);

    let mut session = WsSession::default();
    session.on_bytes(&bytes).unwrap();

    let received = session.close_received().unwrap().clone().unwrap();
    assert_eq!(received.code, 1000);
    assert_eq!(received.reason, "bye");

    let out = session.take_outbound();
    assert_eq!(out[0], 0x80 | CLOSE);
    assert_eq!(&out[2..4], &1000u16.to_be_bytes());
    assert!(session.is_closed());
}

#[test]
fn writes_after_close_are_dropped() {
    let mut session = WsSession::default();
    session.send_close(CLOSE_NORMAL, "done");
    let _ = session.take_outbound();

    session.write(b"too late");
    assert!(
        !session.has_outbound(),
        "data must not follow the closing handshake"
    );
}

#[test]
fn close_frame_with_a_single_byte_payload_is_rejected() {
    // Two bytes are needed for a status code, so one byte is malformed.
    let bytes = client_frame(CLOSE, true, &[0x03], KEY);
    assert_eq!(decode_all(&bytes).unwrap_err(), WsError::InvalidCloseFrame);
}

#[test]
fn empty_close_frame_is_allowed() {
    let bytes = client_frame(CLOSE, true, &[], KEY);
    assert_eq!(decode_all(&bytes).unwrap(), vec![Event::Close(None)]);
}

#[test]
fn protocol_errors_map_to_sensible_close_codes() {
    assert_eq!(WsError::ReservedBitSet.close_code(), 1002);
    assert_eq!(WsError::UnexpectedContinuation.close_code(), 1002);
    assert_eq!(WsError::TextFrameRejected.close_code(), 1003);
    assert_eq!(WsError::BufferOverflow { limit: 1 }.close_code(), 1009);

    let mut session = WsSession::default();
    session.send_close_for(&WsError::TextFrameRejected);
    let out = session.take_outbound();
    assert_eq!(&out[2..4], &1003u16.to_be_bytes());
}
