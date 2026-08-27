# hcr_ws

Sans-io RFC 6455 WebSocket framing, sized for the MQTT-over-WebSocket binding.

Browsers cannot open raw TCP sockets, so a browser MQTT client has to speak MQTT
inside WebSocket frames. This crate is that framing layer.

## Sans-io

Nothing here performs IO or touches an async runtime. `WsSession` takes bytes
and returns bytes. Three things follow from that: the framing logic is testable
without sockets or a runtime, it can be driven by hotaru's
`HotaruRead`/`HotaruWrite`, by tokio, or by anything else without noticing the
difference, and it builds on `no_std + alloc`.

The hotaru `ConnStream` adapter is deliberately kept outside this crate. Because
`MqttServerProtocol<W, TS, Rt>` is generic over its stream, wiring MQTT over
WebSocket needs only a `ConnStream` implementation, with no change to
`hotaru_mqtt`.

## What the MQTT binding adds to RFC 6455

The MQTT specification's WebSocket binding imposes three rules beyond plain
RFC 6455, and this crate enforces all three:

1. The subprotocol is negotiated as `mqtt` (`HandshakeRequest::accept_mqtt`).
2. Payloads travel in binary frames. A text frame is a protocol error
   (`WsError::TextFrameRejected`).
3. Frame boundaries carry no meaning. One MQTT packet may span several frames,
   and one frame may carry several packets, so `WsSession` presents the payload
   as a flat byte stream rather than as discrete messages.

## Usage

```toml
[dependencies]
hcr_ws = "0.3"
```

```toml
[dependencies]
hcr_ws = { version = "0.3", default-features = false }
```

| Feature | Default | Effect |
| --- | --- | --- |
| `std` | yes | Enables `std` in `sha1` and `base64`. Without it the crate is `no_std + alloc`. |

## Requirements

Rust 1.85, edition 2024.

## License

MIT
