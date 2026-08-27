# 04 — Backend Service on hotaru

## 1. Crate layout

```text
hcr-backend/                    # cargo workspace
├── hcr_contract/               # DTOs + serde          [no_std + alloc]
├── hcr_sim/                    # deterministic engine   [no_std + alloc]
│     kinematics, head collision, voxel sweep, scoring, IR interpreter
├── hcr_ws/                     # RFC 6455 ConnStream for hotaru   [std]
├── hcr_qbank/                  # arona integration      [std]
│     HcrDynamicBank, ChallengeContent, difficulty model, calibration
├── hcr/                # the hotaru app         [std, tokio]
│     HTTP + MQTT + WS, broker policy, session actors, replay pool
└── hcr_firmware/               # embassy client         [no_std]  → 05-EMBEDDED.md
```

The split is load-bearing, not cosmetic:

- `hcr_contract` and `hcr_sim` are `no_std + alloc` so the **firmware and the server share one definition**
  of the wire and one implementation of the IR interpreter. That is what makes "one IR, three executors"
  true rather than aspirational.
- `hcr_qbank` must be `std` because arona is `std`-only (`arona/Cargo.toml` — no `#![no_std]`, no feature
  flags, uses `std::time` / `HashMap` / `thread_rng`).
- Build the firmware **separately**: `cargo build -p hcr_firmware --target …`. A `--workspace` build mixing
  std and no_std trips hotaru's own `compile_error!` guards (`hotaru/Cargo.toml:33-58`).

## 2. Protocol assembly

HTTP and MQTT coexist in one registry on one port — this is verified working in hotaru's own test suite
(`hotaru_mqtt_broker/tests/integration.rs:1910-1916`):

```rust
let broker = Broker::<TcpStream>::with_authenticator(Arc::new(HcrAuthenticator::new(store.clone())))
    .with_acl_checker(Arc::new(HcrAcl))
    .with_tenant_resolver(Arc::new(HcrTenants))
    .with_broker_safety(BrokerSafety::default());

let registry: ProtocolEntryRegistry<TcpTransport> = ProtocolRegistryBuilder::new()
    .protocol(ProtocolEntryBuilder::new(HTTP::server(HttpSafety::default())))
    .protocol(ProtocolEntryBuilder::new(MQTT_SERVER::new()))
    .build();

let mut statics = Locals::new();
statics.set(BROKER_STATICS_KEY, broker.clone());
statics.set(HCR_STATE_KEY, app_state.clone());
let runtime = Arc::new(RuntimeConfig::from_parts(Default::default(), Default::default(), statics));
```

Protocol demultiplexing is `Protocol::detect(&[u8])` (`hotaru_core/src/protocol/protocol.rs:108`): HTTP
begins with a method token, MQTT with the CONNECT fixed header `0x10`. Unambiguous.

**Ports.** One port is convenient in development. In production, split them — `:443` HTTPS + WSS for
browsers, `:8883` MQTTS for devices — so that TLS policy, rate limits and firewall rules can differ per
audience. Both listeners share the same registry and the same `Broker`.

## 3. MQTT over WebSocket (`hcr_ws`)

This is the one substantial piece of new infrastructure, and the thing that makes a browser a first-class
MQTT client.

**Why it works without touching `hotaru_mqtt`:** `MqttServerProtocol<W, TS, Rt>` is generic over the stream
(`hotaru_mqtt_broker/src/protocol.rs:74-209`), and `hotaru_mqtt` reaches IO only through
`HotaruRead`/`HotaruWrite`. A WebSocket is a byte-stream carrier. So a `ConnStream` implementation that
frames and de-frames is the entire delta:

```rust
pub struct WsStream<W: ConnStream> { /* … */ }

impl<W: ConnStream> ConnStream for WsStream<W> {
    type ReadHalf  = WsReadHalf<W::ReadHalf>;    // impl HotaruRead:  de-frame, unmask, handle ping/close
    type WriteHalf = WsWriteHalf<W::WriteHalf>;  // impl HotaruWrite: emit binary frames
    type Meta      = W::Meta;
    fn split(self) -> (Self::ReadHalf, Self::WriteHalf, Self::Meta);
}

// then simply:
type MQTT_WS = MqttServerProtocol<WsStream<TcpStream>, WsTransport<TcpTransport>, TokioRuntime>;
```

**Handshake path**, using hotaru's existing upgrade machinery:

```text
GET /mqtt  Upgrade: websocket  Sec-WebSocket-Protocol: mqtt
   → hotaru_http replies 101 SWITCHING_PROTOCOLS          (StatusCode::SWITCHING_PROTOCOLS exists)
   → ConnectionStatus::SwitchProtocol(TypeId::of::<MqttOverWs>())
        (hotaru_core/src/connection/connection.rs:9-25)
   → serve_upgrade(runtime, reader, writer, meta, params, locals)
        (hotaru_core/src/executable/entry/traits.rs:34-42)
   → wrap the buffered halves in WsStream, run MqttServerProtocol on top
```

Requirements the implementation must meet (from RFC 6455 and MQTT's WebSocket binding):

- Subprotocol MUST be negotiated as `mqtt`; reject the handshake otherwise.
- `Sec-WebSocket-Accept` = base64(SHA-1(key ‖ RFC 6455 GUID)).
- **Binary frames only.** A text frame is a protocol error under the MQTT binding.
- **Client→server frames are masked; server→client frames must not be.** Unmask on read.
- Frame boundaries carry no meaning: an MQTT packet may span frames, and one frame may contain several
  packets. The read half must therefore behave as a pure byte stream — which is exactly what makes the
  `HotaruRead` adapter the natural shape.
- Handle continuation frames, ping/pong (reply to ping, and drive keep-alive), and the close handshake.
- Bound the maximum frame and message size, tied to `MqttSafety::max_packet_size` (1 MiB default), so an
  unauthenticated peer cannot force a large allocation.

**Cost and alternative.** This is roughly 600–900 lines plus a conformance test suite (Autobahn is the
standard one). If that is not wanted, the fallback is running an external broker with a WebSocket listener
(mosquitto or EMQX) and having `hcr` connect to it as an ordinary MQTT client. That trades the
hotaru broker away — worth stating plainly, since the project's direction is to use hotaru.

## 4. Broker policy

Implement the four hooks the broker already exposes (`hotaru_mqtt_broker/src/traits.rs:66-215`):

```rust
impl Authenticator for HcrAuthenticator {
    fn authenticate(&self, tenant, connect: &ConnectPacket, remote) -> AuthResult {
        // browsers: username = userId, password = JWT  → verify signature, exp, tenant claim
        // devices:  per-device credential → PBKDF2-SHA512 (broker ships this; bcrypt behind `auth-bcrypt`)
    }
}

impl AclChecker for HcrAcl {
    fn check_publish(&self, ctx, topic)   -> AclDecision { /* the three rules below */ }
    fn check_subscribe(&self, ctx, filter)-> AclDecision { /* … */ }
}
```

The three ACL rules, each closing a specific hole:

1. A user may subscribe only under `hcr/v1/{t}/u/{ownId}/#` — otherwise sessions, scores and item
   selections leak between learners.
2. A device may publish only under `hcr/v1/{t}/dev/{ownId}/up/#` (plus its `lwt`) — otherwise one arm can
   forge another's telemetry, including a "collision cleared" event.
3. `replyTo` in an RPC envelope must resolve inside the caller's own reply subtree — otherwise replies can
   be redirected into someone else's inbox.

Rule 3 is application-level, checked in the RPC handler, because the broker sees `replyTo` only as opaque
payload.

`Broker::new()` is fail-closed (`DenyAllAuthenticator`, `broker.rs:304-314`). `Broker::insecure()` exists
for local experiments and must be impossible to enable in a deployed build — gate it behind a
`#[cfg(debug_assertions)]` or a dev-only feature.

## 5. Session actors

arona's `Session` takes `&mut self` and is not internally synchronized (`arona/src/session/session.rs`), so
each session needs a single owner:

```text
SessionRegistry: DashMap<SessionId, mpsc::Sender<SessionCmd>>
   one task per session; commands: Next, Respond, Finalize, Snapshot
   idle timeout (30 min) → persist SessionState + evict
   restore on demand from the store (03-DYNAMIC-QBANK.md §9)
```

Sessions are long-lived (minutes to hours) and very low rate (one message per item), so a task per session
is cheap and removes all locking from the CAT path. It also gives a natural place to hang the
replay-then-score ordering described in [`03-DYNAMIC-QBANK.md`](03-DYNAMIC-QBANK.md) §3.

## 6. Replay pool

Replay is CPU-bound: up to 500 commands, each sweeping the tool against thousands of voxels.

- Never run it on the async runtime. Use `spawn_blocking` or a dedicated rayon pool sized to
  `cores - 1`.
- **Bounded queue with backpressure.** When full, return `RATE_LIMITED` rather than growing memory.
- **Per-submission fuel limit**: max simulated ticks and a 5 s wall-clock budget → `REPLAY_TIMEOUT`. Without
  this, a program with maximal repeats against a dense voxel field is a denial-of-service primitive.
- Cache by `(challengeId, challengeVersion, programHash)`. Identical resubmissions are common — retries,
  idempotent replays, and shared classroom solutions — and the result is deterministic by construction
  ([`02-DETERMINISM.md`](02-DETERMINISM.md)).

## 7. Catalog store

- Item content, IRT parameters and calibration state are versioned rows; `challengeVersion` is immutable
  once served.
- `CatalogSnapshot` is an immutable `Arc` swapped atomically; `HcrDynamicBank` holds one for the life of a
  selection so the pool cannot change mid-decision.
- Generated items are materialized on first use and stored with full provenance
  (`familyId`, `version`, `seed`, `params`) so they can be reproduced exactly.

## 8. Device bridge

The backend hosts the broker, but the broker exposes no public "subscribe from inside the process" hook —
its subscription tree is internal (`hotaru_mqtt_broker/src/broker.rs:88-177`). The honest options:

| Option | Notes |
| --- | --- |
| **Loopback MQTT client** (recommended) | `hcr` runs an internal `MQTT::new()` client over loopback TCP subscribing `hcr/v1/+/dev/+/up/#`. Public API only, robust, one loopback hop |
| In-process subscription hook | Cheaper, but requires an upstream addition to `hotaru_mqtt_broker` |

Co-location still removes the external broker dependency entirely; the remaining cost is a loopback socket,
not a network round trip. Worth being precise about rather than overselling "same process = free".

Telemetry mirroring to browsers is then a topic re-publish: device telemetry arrives on
`dev/{id}/up/telemetry`, the bridge downsamples (≤10 Hz for display) and republishes to the watching user's
`u/{uid}/evt/telemetry`. Browsers never subscribe to device topics directly — that keeps ACL rule 1 simple
and lets the backend enforce who may watch which arm.

## 9. Observability

Minimum viable signals, each tied to a failure this design anticipates:

| Signal | Catches |
| --- | --- |
| `replay_divergence_jaccard` histogram | TS/Rust engine drift ([`02`](02-DETERMINISM.md) §4) |
| `replay_duration_ms`, `replay_queue_depth` | Replay pool saturation before it becomes `RATE_LIMITED` |
| `item_exposure_rate` per item | Bank burn; Sympson–Hetter tuning |
| `item_residual` per item | Calibration drift → retirement |
| `session_se_at_termination` | Whether the bank actually covers the θ range |
| `device_offline_transitions` | Flaky Wi-Fi on the arm, LWT storms |
| `bank_exhausted_total` | Generation failing to keep up with demand |

## 10. Build and dependency notes

- **The `hotaru_core` feature mismatch is fixed** (2026-07-31): `hotaru_mqtt` requested a `full` feature
  that was renamed to `full_regex` upstream. Both call sites patched; `hotaru_mqtt` now builds and its 188
  tests pass. The change is uncommitted in that checkout and should be upstreamed. Note the manifests still
  pin `hotaru_core 0.8.3` against a `0.8.4` tree — path deps mask that, a published build would not.
- `hotaru_mqtt` default features (`std`, `spawn_send`) are correct for the server; the broker crate is
  std-only by construction.
- TLS via `hotaru_tls` (`MQTTS` / `MQTTS_SERVER` aliases) once certificates exist.
- Both `h2per` and `hotaru_grpc` are excluded from the hotaru workspace (`hotaru/Cargo.toml:20-21`) and
  should be treated as unavailable.
