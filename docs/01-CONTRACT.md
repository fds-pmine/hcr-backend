# 01 — Frontend / Backend Contract (`hcr.v1`)

Normative types: [`schema/hcr-v1.d.ts`](schema/hcr-v1.d.ts) (TypeScript) and
[`schema/hcr_v1.rs`](schema/hcr_v1.rs) (Rust mirror). When prose and schema disagree, **the schema wins**.

## 1. Identity, versioning, compatibility

- Protocol id: **`hcr.v1`**. Versioned independently of app and firmware versions.
- `v` (major) appears in both the topic tree (`hcr/v1/...`) and the envelope. A major bump means a parallel
  topic tree; the old tree keeps running until clients drain.
- **Minor evolution is additive only**: new optional fields, new `kind` values, new topics. Receivers MUST
  ignore unknown fields and MUST NOT fail on an unknown `kind` — log and drop.
- Wire field naming is `camelCase` everywhere, including on the Rust side
  (`#[serde(rename_all = "camelCase")]`), so the TS types are literal.
- Reused v1 types (`ScoreInput`, `ScoreResult`, `ProgramMetrics`, `ChallengeSummary`, `Program`,
  `RobotCommand`) are **copied verbatim** from `src/types/domain.ts` and
  `src/features/blockly/programTypes.ts`. They are frozen by SPEC v0.3 §15 and must not drift.

## 2. Envelope

Every MQTT payload is one `Envelope`. Nothing is ever published bare.

```ts
interface Envelope<K extends string = string, P = unknown> {
  v: 1;
  id: string;          // ULID, unique per message; the idempotency key
  kind: K;             // discriminator, e.g. "session.next.req"
  ts: number;          // sender epoch-ms, informational only — never used for ordering
  corr?: string;       // correlation: echoes the `id` of the request being answered
  replyTo?: string;    // topic the responder must publish the reply to
  src?: ActorRef;      // { type: 'user'|'device'|'service', id: string }
  payload: P;
}
```

Design notes:

- **Correlation lives in the envelope, not in MQTT 5 properties.** `hotaru_mqtt` supports v5
  (`packet.rs:34-59`), but devices may negotiate 3.1.1 and the HTTP binding has no MQTT properties at all.
  One mechanism that works everywhere beats two that each work sometimes.
- `ts` is explicitly **not** an ordering key. Device clocks are unreliable (the ESP8266 only gets NTP when
  the router is reachable, `ESP8266.ino:506-513`). Ordering comes from MQTT's per-topic FIFO guarantee.
- `id` doubles as the idempotency key (§8).

## 3. Transport bindings

| Client | Transport | Encoding |
| --- | --- | --- |
| Browser | **MQTT over WebSocket** (`wss://…/mqtt`, subprotocol `mqtt`, binary frames) | JSON |
| Rust firmware (target state) | MQTT over TCP/TLS | CBOR |
| ESP8266 (today, via gateway) | not MQTT — gateway translates, see [`05-EMBEDDED.md`](05-EMBEDDED.md) | text command language |
| Any HTTP client / compat path | HTTPS request-response | JSON |

### 3.1 MQTT over WebSocket (browser)

Per the MQTT specification's WebSocket binding: subprotocol MUST be `mqtt`, payloads travel in **binary**
frames, and a WebSocket message boundary carries no meaning — an MQTT packet may span frames and a frame may
hold several packets. The read side must therefore treat frames as a byte stream, which is exactly what a
`ConnStream` adapter does ([`04-SERVICE.md`](04-SERVICE.md) §3).

### 3.2 HTTP binding (compatibility)

The HTTP binding exists so the two existing frontend interfaces keep working unchanged, and so that
non-browser tooling (curl, CI, load tests) has a path in. It carries envelope metadata in headers:

| Envelope field | HTTP |
| --- | --- |
| `id` | `X-HCR-Message-Id` |
| `corr` | `X-HCR-Correlation-Id` |
| `kind` | implied by route |
| `payload` | request/response body |

```text
GET  /api/v1/challenges                → ChallengeSummary[]   (ordered; see below)
GET  /api/v1/challenges/{id}           → ChallengeDefinitionDto
POST /api/v1/score                     → ScoreResult          (ScoreProvider parity)
POST /api/v1/submissions               → SubmissionAccepted | SubmissionResult
GET  /api/v1/submissions/{id}          → SubmissionResult
POST /api/v1/sessions                  → SessionSnapshot
POST /api/v1/sessions/{id}/next        → NextItem
POST /api/v1/sessions/{id}/responses   → ResponseOutcome
POST /api/v1/sessions/{id}/finalize    → SessionResultDto

GET  /api/v1/time                      → TimeSync             (clock offset, §06 §5)
POST /api/v1/matches                   → MatchState
GET  /api/v1/matches/{id}              → MatchState
GET  /api/v1/matches/{id}/challenge    → ChallengeDefinitionDto  (409 before T0)
POST /api/v1/matches/{id}/join         → MatchState
POST /api/v1/matches/{id}/start        → MatchState
POST /api/v1/matches/{id}/submissions  → MatchSubmissionAck      (never a score)
GET  /api/v1/matches/{id}/results      → MatchResults            (409 until close)
```

Round endpoints carry two more headers:

| Header | Meaning |
| --- | --- |
| `X-HCR-Player` | Authenticated identity. **Written by the auth layer**, which overwrites whatever the client sent. It decides what a caller may *do*, so a client-chosen value would let anyone act as anyone. |
| `X-HCR-Player-Name` | Display name for the roster and leaderboard. Cosmetic, so the client may choose it. Falls back to the player id when absent. |

`examples/serve` has no auth layer and therefore trusts `X-HCR-Player` as sent. That is a property of a
development server, not of the binding.

**Listing order is normative.** `GET /api/v1/challenges` returns hand-authored challenges first, then
generated ones, each group by id. Reproducibility is only half the reason: a client with no other signal
opens the *first* entry, and a plain id sort made that an accident of the alphabet — generated ids begin
`cap-trim-…`, so a provisional machine-made item outranked the authored challenge it was generated from.
A client that needs a specific item must still name it; this ordering makes "the first one" a defensible
default, not a substitute for asking. An unpinned competitive round applies the same rule, additionally
skipping `retired` items (`CatalogStore::pick_for_match`).

These are the endpoints already reserved at `docs/HCR_Simulator_SPEC_v0.3.md:528-531`, kept identical in
shape so the spec does not need rewriting.

> **`Challenge` vs `ChallengeDefinition`.** The wire type is `ChallengeDefinition`
> (`initialHair.voxels: VoxelCoord[]`), **not** `Challenge` — the latter holds `ReadonlySet<VoxelKey>`,
> which has no JSON form. `HttpChallengeProvider` fetches the definition and runs the *existing* normalizer
> and validator, exactly as `LocalChallengeProvider` does today. Hair voxel arrays are large; rely on
> HTTP content-encoding (`hotaru_lib` ships gzip/brotli/zstd) rather than inventing a packed format.

## 4. Topic tree

```text
hcr/v1/{tenant}/rpc/{service}/{method}      ← client requests      QoS1
hcr/v1/{tenant}/u/{userId}/rep/{connId}     → RPC replies          QoS1
hcr/v1/{tenant}/u/{userId}/evt/{kind}       → server push to user  QoS1

hcr/v1/{tenant}/dev/{deviceId}/up/telemetry → device telemetry     QoS0
hcr/v1/{tenant}/dev/{deviceId}/up/state     → device state         QoS1, retained
hcr/v1/{tenant}/dev/{deviceId}/up/event     → discrete events      QoS1
hcr/v1/{tenant}/dev/{deviceId}/up/ack       → command acks         QoS1
hcr/v1/{tenant}/dev/{deviceId}/dn/cmd       ← commands to device   QoS1
hcr/v1/{tenant}/dev/{deviceId}/dn/estop     ← emergency stop       QoS1
hcr/v1/{tenant}/dev/{deviceId}/dn/cfg       ← config               QoS1, retained
hcr/v1/{tenant}/dev/{deviceId}/lwt          → last will            QoS1, retained

hcr/v1/{tenant}/match/{matchId}/evt/state     → phase + closesAt    QoS1, retained
hcr/v1/{tenant}/match/{matchId}/evt/challenge → revealed at T0      QoS1
hcr/v1/{tenant}/match/{matchId}/evt/results   → final ranking       QoS1
hcr/v1/{tenant}/match/{matchId}/evt/presence  → joins / leaves      QoS1
```

Match topics are membership-scoped: `check_subscribe` must admit only joined participants
([`06-MULTIPLAYER.md`](06-MULTIPLAYER.md) §6).

The `up/` ÷ `dn/` split exists so the ACL is two prefix rules per principal instead of a per-topic list.
`{tenant}` maps onto `hotaru_mqtt_broker`'s `TenantResolver` / `TenantId`, which already scopes the
subscription tree (`hotaru_mqtt_broker/src/broker.rs:82`).

### QoS / retain policy

| Class | QoS | Retain | Why |
| --- | --- | --- | --- |
| Telemetry (10–50 Hz) | 0 | no | Stale samples are worthless; retransmission costs more than it's worth |
| State, config | 1 | **yes** | A late subscriber must immediately learn where the arm is |
| Commands, acks, RPC | 1 | no | Must arrive; duplicates handled by idempotency (§8) |
| Last will | 1 | yes | Offline detection must survive the subscriber being absent |

QoS 2 is deliberately unused. It is supported (`protocol.rs:770-797`) but its only advantage over QoS 1 is
exactly-once, which §8's idempotency keys already provide at lower cost.

## 5. RPC over MQTT

1. Client subscribes to `hcr/v1/{t}/u/{uid}/rep/{connId}` (its own reply inbox).
2. Client publishes an `Envelope` to `hcr/v1/{t}/rpc/{service}/{method}` with `replyTo` set to that inbox
   and a fresh `id`.
3. Backend publishes the reply to `replyTo` with `corr` = the request `id`.
4. Client times out on its own clock (recommended 10 s for control, 60 s for submissions) and may retry with
   the **same `id`** — that is what makes retry safe.

The backend MUST validate that `replyTo` lies inside the caller's own `u/{userId}/rep/` subtree. Without
that check, any authenticated user could direct replies into another user's inbox.

## 6. Message catalog

Full field lists live in the schema files; this is the map.

### 6.1 Catalog

| `kind` | Direction | Payload |
| --- | --- | --- |
| `catalog.list.req` / `.res` | client → server | `{}` / `ChallengeSummary[]` |
| `catalog.get.req` / `.res` | client → server | `{ challengeId, version? }` / `ChallengeDefinitionDto` |

`ChallengeDefinitionDto` = v1's `ChallengeDefinition` plus a `meta` block carrying `version`,
`irt` (item parameters), `dimensions` (skill tags), and generator provenance. `meta` is additive; a client
that ignores it still gets a valid v1 challenge.

### 6.2 Submission and scoring

| `kind` | Payload |
| --- | --- |
| `submission.create.req` | `{ submissionId, challengeId, challengeVersion, program: Program, cutterGrid?, sessionId?, itemRef?, clientPreview? }` |
| `submission.accepted.res` | `{ submissionId, state: 'queued' }` |
| `submission.result.evt` | `SubmissionResult` |

- The client sends `Program.nodes`, **not** `runtimeCommands` (decision D3). The server performs its own
  `repeat` expansion and enforces `MAX_RUNTIME_COMMANDS = 500`
  (`src/features/blockly/programCompiler.ts:12`).
- `clientPreview` (`{ scoreResult, resultVoxelsHash, engineVersion, tickMs }`) is **advisory**. Its only job
  is divergence telemetry: when it disagrees with the replay, that is a conformance bug worth alerting on
  ([`02-DETERMINISM.md`](02-DETERMINISM.md) §4).
- Replay is CPU-bound, so submission is asynchronous by default: acknowledge, then push
  `submission.result.evt`. The HTTP binding may answer synchronously for small programs.
- **`cutterGrid`** is set when the program was written in Cutter Grid rather than with joint angles. It
  carries the player's lattice IR *and* the frozen trajectory the browser planned from it, because a Cutter
  Grid motion is not derivable from its program without redoing the client's compile-time IK search. The
  server verifies that trajectory rather than replaying `program`, which is then empty. Additive and
  optional: a servo submission is unchanged, and a server that ignores the field still speaks a complete v1.
  Rules, limits and the trust boundary: [`08-CUTTER-GRID.md`](08-CUTTER-GRID.md).

### 6.3 Assessment session (CAT)

| `kind` | Payload |
| --- | --- |
| `session.start.req` / `.res` | `{ blueprintId?, initialTheta?, programmingMode? }` / `SessionSnapshot` |
| `session.next.req` / `.res` | `{ sessionId }` / `NextItem` |
| `session.respond.req` / `.res` | `{ sessionId, itemRef, submissionId }` / `ResponseOutcome` |
| `session.finalize.req` / `.res` | `{ sessionId }` / `SessionResultDto` |

`NextItem` carries `{ itemRef, challengeId, challengeVersion, expectedRemaining }`.

**`itemRef`** is an opaque HMAC-signed token binding `(sessionId, bankIndex, challengeId, challengeVersion,
issuedAt)`. It exists because arona identifies items by `Vec` index and `Question` carries no ID
(`arona/src/core/question.rs:159-165`); the token carries that index across the wire without exposing or
trusting it. The server rejects a `session.respond` whose `itemRef` was not the one it last issued for that
session.

Session state machine:

```text
active ──(next)──> awaiting-response ──(respond)──> active
   │                                                   │
   └────────────────(terminate)────────────────────────┘
                          ↓
                     terminated ──(finalize)──> finalized
```

`session.next` on a terminated session returns `SESSION_TERMINATED`, mirroring arona's own behaviour
(`Session::next_question` checks the terminator first, `arona/src/session/session.rs:445`).

### 6.4 Competitive rounds

| `kind` | Payload |
| --- | --- |
| `match.create.req` / `.res` | `{ challengeRef?, durationMs, rankBy, maxPlayers, programmingMode? }` / `MatchState` |
| `match.join.req` / `.res` | `{ matchId }` / `MatchState` |
| `match.leave.req` | `{ matchId }` |
| `match.time.req` / `.res` | `{ clientSentAt }` / `{ clientSentAt, serverTime }` |
| `match.state.evt` | `MatchState` (retained) |
| `match.challenge.evt` | `ChallengeDefinitionDto` — published at T0, never before |
| `match.results.evt` | `MatchResults` |
| `match.presence.evt` | `{ playerId, event: 'join'\|'leave'\|'disconnect' }` |

Submissions reuse `submission.create.req` with `matchId` set. During `running` the server returns only
acceptance — never a score — so no player can read a standing before the round closes. Acceptance is decided
purely by **server receive time** against `closesAt`; the client's `ts` is ignored. Rules and rationale in
[`06-MULTIPLAYER.md`](06-MULTIPLAYER.md) §3.

### 6.5 Device

| `kind` | Direction | Payload |
| --- | --- | --- |
| `dev.state.evt` | device → server | `{ status, firmware, axes: AxisState[], wireFormat, rssi? }` |
| `dev.telemetry.evt` | device → server | `{ tMono, angles: number[], busy }` |
| `dev.event.evt` | device → server | `{ event: 'cmd.start'|'cmd.done'|'fault'|'limit', ... }` |
| `dev.ack.evt` | device → server | `{ corr, ok, error? }` |
| `dev.cmd.req` | server → device | `{ corr, op: 'run'|'home'|'stop'|'resume'|'query', program?, textCmd? }` |
| `dev.estop.req` | server → device | `{ reason }` |
| `dev.cfg.req` | server → device | `{ axes: AxisConfig[], speedLimits, calibration }` |

- `dev.state.evt` is retained and declares `wireFormat: 'json' | 'cbor' | 'text'`. The backend encodes
  subsequent traffic accordingly. This is how one contract serves both a Rust firmware and a translated
  ESP8266 without a content-type property that MQTT 3.1.1 lacks.
- **E-stop is a separate topic**, never queued behind `dn/cmd`. A stop that waits behind a program is not a
  stop. Firmware must handle it in its receive path ahead of ordinary commands.
- Loss of connection (LWT fires) MUST be treated as "position unknown" — the device is required to hold
  position and refuse further motion until it re-registers.

## 7. Error model

```ts
interface HcrError {
  code: string;                        // stable SCREAMING_SNAKE
  message: string;                     // English, human-readable
  retryable: boolean;
  field?: string;                      // for validation errors
  details?: Record<string, unknown>;
}
```

Over MQTT: reply with `kind: "error"` and `corr` set. Over HTTP: non-2xx with `{ error: HcrError }`.

| Code | HTTP | Retryable |
| --- | --- | --- |
| `UNAUTHORIZED` / `FORBIDDEN` | 401 / 403 | no |
| `CHALLENGE_NOT_FOUND` | 404 | no |
| `PROGRAM_INVALID` (with `field`) | 422 | no |
| `PROGRAM_TOO_LARGE` (>500 commands) | 422 | no |
| `WEIGHTS_INVALID` | 422 | no |
| `TRAJECTORY_REJECTED` (with `field`, `details.rejection`) | 422 | no |
| `ITEM_REF_INVALID` | 409 | no |
| `SESSION_NOT_FOUND` / `SESSION_TERMINATED` | 404 / 409 | no |
| `MATCH_NOT_READY` | 409 | yes (later) |
| `BANK_EXHAUSTED` | 409 | no |
| `DEVICE_OFFLINE` / `DEVICE_BUSY` | 409 | yes |
| `REPLAY_TIMEOUT` | 504 | yes |
| `RATE_LIMITED` | 429 | yes |
| `INTERNAL` | 500 | yes |

`PROGRAM_INVALID` carries `field` so the frontend can keep its existing behaviour of highlighting the
offending block (SPEC v0.3 §13.2) — the compiler already tracks `sourceBlockId` on every command.

`TRAJECTORY_REJECTED` is separate from `PROGRAM_INVALID` because the two need different fixes. The former
means the program is malformed; the latter means the program is fine and the trajectory planned from it did
not survive server-side verification, which the learner did not cause and cannot correct by editing blocks.
`details.rejection` names which audit failed ([`08-CUTTER-GRID.md`](08-CUTTER-GRID.md) §6).

`MATCH_NOT_READY` covers the two refusals that are the *design* of a round rather than a fault: the
challenge during the lobby, and results while the round is still running. Both mean "try again later, not
differently", which is why they are not `ITEM_REF_INVALID` (nothing was forged) or `SESSION_TERMINATED`
(nothing has ended). Its `message` is written to be shown to a waiting player.

## 8. Idempotency, ordering, delivery

- **QoS 1 is at-least-once**, so every mutating message MUST be idempotent by `id`. The backend keeps a
  dedupe window (recommended 24 h for submissions, 60 s for device commands) keyed on `id`, returning the
  original result on replay rather than re-executing.
- `submissionId` is client-generated (ULID) and is the idempotency key for scoring. Re-submitting returns
  the first result; it never re-scores.
- Ordering is per-topic FIFO only. Never rely on cross-topic ordering — telemetry (QoS 0) and events (QoS 1)
  travel independently and will interleave.
- The broker preserves publisher ordering per connection (`hotaru_mqtt`'s per-endpoint FIFO dispatcher,
  `protocol.rs:549-645`), so a device receiving `dn/cmd` twice sees them in send order.

## 9. Authentication and authorization

| Principal | Authenticates via | May publish | May subscribe |
| --- | --- | --- | --- |
| Browser user | JWT in MQTT CONNECT password (username = `userId`); `Authorization: Bearer` on HTTP | `hcr/v1/{t}/rpc/#` | `hcr/v1/{t}/u/{ownId}/#` |
| Device | Per-device credential (broker ships PBKDF2-SHA512; bcrypt behind `auth-bcrypt`) | `hcr/v1/{t}/dev/{ownId}/up/#`, `…/lwt` | `hcr/v1/{t}/dev/{ownId}/dn/#` |
| Backend service | Internal credential | all | all |

Implemented with the broker's existing hooks: `Authenticator` for CONNECT, `AclChecker` for
publish/subscribe, `TenantResolver` for `{tenant}` (`hotaru_mqtt_broker/src/traits.rs:66-115`).
`Broker::new()` is fail-closed by default; `Broker::insecure()` must never reach a deployed environment.

Three rules the ACL must enforce, each corresponding to a real attack:

1. A user may only subscribe under its own `u/{userId}/` — otherwise sessions and scores leak sideways.
2. A device may only publish under its own `dev/{deviceId}/up/` — otherwise one arm can forge another's
   telemetry, including "collision cleared".
3. `replyTo` must be inside the caller's own reply subtree (§5).

## 10. Limits

| Limit | Value | Source |
| --- | --- | --- |
| Max runtime commands per program | 500 | `programCompiler.ts:12` (v1 invariant) |
| Max MQTT packet | 1 MiB (server default); 8 KiB on MCUs | `MqttSafety::max_packet_size` |
| Max challenge payload | 2 MiB uncompressed | new |
| Max request body | 8 MiB | sized for a Cutter Grid trajectory at the command cap (~2.5 MB); `deploy.rs` and both proxy configs must agree |
| Max waypoints per Cutter Grid plan | 120 000 | verification cost guard |
| Replay wall-clock budget | 5 s per submission | new |
| Telemetry rate | ≤50 Hz, server may downsample before mirroring | new |
| RPC timeout | 10 s control / 60 s submission | new |
