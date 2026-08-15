# 05 — Embedded Implementation

## 1. What the hardware actually is

`ESP8266.ino` is the seller-supplied firmware for a Taobao 4/5/6-axis servo arm kit
(`ESP8266.ino:33-35`). Reviewing it changes the embedded plan substantially.

| Property | Value | Source |
| --- | --- | --- |
| MCU | **ESP8266** (Xtensa LX106), Arduino framework | `ESP8266.ino:39` |
| Active axes | `ss = 5` → `X, Y, Z, B, E` (base → gripper); `T` spare | `:92-94` |
| GPIO | `{5, 4, 0, 16, 14, 12}` | `:94` |
| Servo range | 0–180°, home 90°; `E` restricted to 45–100° | `:95-99` |
| PWM | 500–2500 µs, `Servo.attach(pin, 500, 2500)` | `:127-138, 425` |
| Motion | Blocking; `delay()` until the move completes, capped at `Maxdms = 1440 ms` | `:177-180` |
| Effective speed | `ms += MAX * 10` → ~**100 °/s** | `:177` |
| Watchdog | Long loops need `yield()` or the soft WDT reboots the board | `:168` |
| Config | `/config.json` on SPIFFS | `:325-386` |
| Transports | HTTP :80, TCP :8000, UDP :8888, Serial, Blinker cloud, captive-portal DNS | `:540-552` |
| Outbound dial | Can **connect out** to `SIP:SPort` every 2 min when idle | `:594-596` |
| MQTT | **none** | repo-wide search |

### The command language

The arm speaks text, not IR (`ESP8266.ino:741-745`):

```text
X 50            absolute: axis X to 50°
X +10 / X -10   relative
X ?             random angle
X 50;Y 90;Z 10  multiple commands, ';' separated (executed together via ServoGo(-1,0))
H               home all axes to Raw[]
?               query → {"X":90,"Y":90,"Z":90,"B":90,"E":90,"Cmd":0}
Stop 0 | -1 | N clear queue / pause indefinitely / pause N ms
Start           resume
delay N, D      delay
step            interpolation step size
sync 0|1        async / sync execution
Time …          scheduled execution
DIR, format, F, C, S, R, RE, Test, SH, /   file ops on SPIFFS, restart, self-test
```

Verbs enumerated from the dispatcher at `ESP8266.ino:826-1163`; the file-operation verbs were not traced
exhaustively and should be confirmed before being relied on.

## 2. Why this changes the plan

The stated goal is a hotaru + MQTT embedded implementation. Two facts block a direct route:

1. **The arm has no MQTT.** Adding it means changing the firmware.
2. **Rust does not practically target the ESP8266.** `esp-hal` and the embassy ecosystem cover the
   ESP32/S2/S3/C3/C6/H2 families; the ESP8266's Xtensa LX106 is served only by the largely unmaintained
   `esp8266-hal`, with no embassy time-driver story. `hotaru_rt_embassy` + `hotaru_io_embedded` therefore
   have no viable ESP8266 target. *(Worth re-verifying against current crate status before committing —
   this is an ecosystem fact, not a permanent law.)*

So the plan is **staged**: get the physical arm working now without touching its firmware, and land the
Rust/hotaru firmware on hardware that can actually run it.

## 3. Joint mapping — one remaining mismatch

**Angles on the wire are servo degrees**, not geometric ones — what the arm is actually commanded to, and
what `hcr-fw` reports back. `JointConfig.min/maxAngleDeg` are therefore directly comparable with the
firmware's own limits, which is what makes the table below a check rather than a conversion exercise.

Firmware limits are `AXES` in `hcr-fw/hcr-gateway/src/robot/axis_config.rs`, converted from tenths of a
degree. Every axis homes at 90°.

| Simulator joint | Servo range | Geometric | Axis | Firmware travel | Status |
| --- | --- | --- | --- | --- | --- |
| `baseYaw` | 30…150 | −60…60 | `X` | 0…180 | ✅ fits, 30° margin each side |
| `shoulder` | 30…150 | −20…100 (centre 90, offset 40) | `Y` | 0…180 | ✅ fits, 30° margin each side |
| `elbow` | 17.5…162.5 | −135…10 (centre 90, offset −62.5) | `Z` | 0…180 | ✅ fits, 17.5° margin each side |
| `wrist` | 0…180 | −90…90 | `B` | 0…180 | ✅ fits exactly — **no margin** |
| `shoulderRoll` | — | −45…45 | *(none)* | — | ❌ **no hardware axis** |
| — | — | — | `E` (cutter) | 45…100 | ❌ **no simulator joint** |

The conversion is a single affine map, defined once in `AxisConfig::to_servo_deg`
([`schema/hcr_v1.rs`](schema/hcr_v1.rs)):

```
servoDeg = clamp(centerDeg + direction × (jointDeg − offsetDeg), minDeg, maxDeg)
```

`crates/hcr_sim/tests/servo_travel.rs` encodes this table so a range edit that overruns real servo travel
fails in CI. That matters more than it used to: the Cutter Grid planner searches for poses *near* the joint
limits, because that is where the reach is, so a range overstating the hardware produces certified
trajectories the arm cannot fly.

**`wrist` was the third mismatch and is now resolved.** It used to span −100…100 geometric, which needs 200°
of travel from a 180° servo; the arm would have clamped and silently disagreed with the screen at the
extremes. It is now −90…90, the full servo throw and no more. The remaining two need product decisions, not
code:

1. **`shoulderRoll` has no hardware axis.** Either constrain hardware-bound challenges to
   `shoulderRoll = 0`, add a sixth servo (`T` on GPIO 12 is free, and `ss` is configurable), or mark
   roll-using challenges simulation-only.
2. **The cutter `E` is unmodelled.** v1 explicitly excludes scissor open/close (`AGENTS.md`, "v1
   prohibitions"), so `E` stays parked at home — `servo_travel.rs` asserts no joint claims it. If cutting is
   ever modelled, `E` is where it lands. Note that Cutter Grid does not change this: its tool is always
   cutting, which is a property of how hair removal is modelled, not a servo that opens and closes.

`hardwareCompatible` in `ChallengeMeta` is computed from these rules at generation time, so the qbank never
serves a physically impossible challenge to a hardware-backed session.

**Timing will not match.** The simulator models 45–75 °/s per joint; the kit runs at ~100 °/s with a blocking
delay. `ProgramMetrics.estimatedDurationMs` is a *simulation* quantity used for the Time score — it must not
be compared against hardware wall-clock. Real per-axis speeds belong in `dev/cfg` (`AxisConfig.speedDegPerSec`),
measured on the actual arm.

## 3.1 Cutter Grid on the arm: destinations, not the path

Servo programs map onto the hardware almost directly. Cutter Grid does not, and the reason is the second
mismatch above rather than anything about the mode itself.

**The ladder planner uses `shoulderRoll` as a real degree of freedom.** Measured over the certified reference
trajectory: 2,095 distinct roll angles spanning −11.4° to +45°, across all 2,134 waypoints. It is how the arm
reaches around the head. The hardware has five servos and none of them rolls the shoulder.

There is no partial version of that to send. Pinning the roll at rest and playing the other four joints puts
the tool tip up to **0.50 m** from the planned path — **3.1 voxels, 4.2 tool radii** — with a mean error of
0.18 m. That is not a degraded cut; it is a different motion. The arm would move confidently along a path
with no relation to the screen, which is the single failure `armBridge.ts` exists to prevent.

So `buildCutterArmPlan` — the full-path replay — **refuses**, and reports the measured deviation rather than
asserting the rule. What the dock actually sends is the endpoint plan below, which sidesteps the constraint
entirely.

### What runs instead: endpoints, not the path

The constraint above comes entirely from insisting the arm reproduce the *planner's* joint path. It does
not have to.

`Move left 3 voxels` is one instruction with one destination, and on hardware **there is no hair** — nothing
depends on the route between destinations. So the arm is given the position each block ends at and solves
its own pose for each, with the roll pinned at rest, using the same certified DLS solver the planner uses
(pinning is done by setting that joint's range to a single value, so the solver's own limit clamp enforces
it — no second solver, no fork).

Measured on the shipped challenge's reference program:

| | Result |
| --- | --- |
| Block endpoints | **5/5** reachable roll-free and clear of the head |
| Individual cell centres | **22/22** reachable, if a finer trace is ever wanted |
| Landing accuracy | within a quarter voxel of each destination |
| Arm steps | 5, one per block |

`buildCutterArmEndpointPlan` is what the dock sends. It refuses whole — sending nothing — if any single
destination cannot be solved, rather than driving to the ones it can and stopping somewhere arbitrary.

**This is not a replay and is not scoreable.** The arm visits the same cells by its own means; the swept
volume between them is whatever the servos do, not the simulated one. That is fine for driving hardware and
wrong for anything that measures a cut, which is why scoring stays on the server.

### What full-path replay would still need

A sixth servo on `T` (GPIO 12, free and already configurable — §3). `buildCutterArmPlan` implements that
route and is tested against the real planner output, so it becomes available the moment the axis exists:

- **Multi-axis `pose` steps.** A Cutter Grid waypoint moves every joint at once. `arm.setAngles` has always
  accepted an array and built `?X=..&Y=..&Z=..&B=..`; nothing had reason to pass more than one element until
  now. One request per waypoint instead of four.
- **Error-bounded decimation.** 2,134 waypoints a few milliseconds apart is far past the 512-step budget and
  far faster than an ESP8266 answers HTTP. Poses are chosen so the interpolated path stays within a stated
  joint tolerance of the frozen trajectory, tightened to the smallest bound that fits the budget.
- **Duration stretching.** Each step gets the longer of its planned duration and the time the servos need
  (`Maxdms = 1440` for 180°, so 8 ms/degree). The path is preserved; the clock gives. Sending the next pose
  while the arm is still travelling would make it silently cut corners.

Measured on the reference trajectory with the roll frozen, at the 512-step budget:

| | Value |
| --- | --- |
| Waypoints → poses | 2,112 → **277** (13.1%) |
| Joint tolerance | **0.1°** — the firmware's own resolution, so nothing was given up |
| Tool-tip error | **3.3 mm**, against a 120 mm tool radius |
| Duration | 13.6 s planned → 15.1 s on the arm (×1.11) |

The decimation is effectively lossless at the resolution the hardware can represent. The blocker is the
missing axis, and only that.

One caveat if the axis ever arrives: `/api/angles` applies the servos **in `X, Y, Z, B, E` order,
sequentially** (`docs/API.md`) — one request, not one simultaneous motion. Cutter Grid trajectories are
synchronised multi-joint paths, so the arm stair-steps them. At these per-step deltas the difference is below
the enforced tolerance, but it is not a synchronised move and the vendor docs warn that driving several
servos at once "draws a lot of power, needs external supply".

## 4. Tier 0 — gateway, no firmware change (recommended first step)

```mermaid
flowchart LR
    B["Browser<br/>MQTT/WS"] --> S["hcr_service<br/>broker + gateway"]
    S -->|"TCP :8000<br/>'X 50;Y 90;'"| E["ESP8266<br/>stock firmware"]
    E -->|'{\"X\":90,…}'| S
```

- The gateway translates Program IR → the text command language, and parses the `?` JSON reply back into
  `dev.telemetry.evt` / `dev.state.evt`.
- MQTT terminates at the gateway; the device profile advertises `wireFormat: 'text'`, which the contract
  already supports ([`01-CONTRACT.md`](01-CONTRACT.md) §6.4).
- **Use the arm's outbound dial** (`SIP`/`SPort`, `ESP8266.ino:594-596`): point it at the gateway and the arm
  connects out every 2 minutes when idle. That removes any need for the arm to be reachable inbound —
  no port forwarding, no static IP, works behind NAT.
- Translation is direct: each `set-joint-angle` becomes `AXIS deg`, batched with `;` into a single line;
  `wait N` becomes `delay N`.
- Emulate the LWT contract in the gateway: when the TCP session drops, publish the retained offline state on
  the device's behalf.

Limitations to be honest about: the stock firmware gives no per-command completion callback (moves are
blocking and the reply is a state snapshot), so `cmd.done` events are inferred from the reply, not observed.
There is also no e-stop that pre-empts an in-flight blocking move — `Stop 0` clears the *queue*, but the
current `ServoGo` call runs to completion.

## 5. Tier 1 — MQTT in Arduino (optional)

If the arm should speak MQTT natively before Rust hardware exists: add PubSubClient to the seller firmware,
subscribe `hcr/v1/{t}/dev/{id}/dn/#`, publish `up/#`, keep payloads in the existing text command language
(tiny, and the parser already exists). This is not hotaru — it is a pragmatic intermediate that removes the
gateway's translation hop while leaving the arm's semantics unchanged.

## 6. Tier 2 — hotaru firmware on ESP32-class hardware

This is where the requested hotaru embedded implementation actually lands: a new controller board
(ESP32-S3 or C3) driving the same servos.

```toml
[dependencies]
hotaru_mqtt      = { version = "0.8", default-features = false, features = ["embedded", "spawn_send"] }
hotaru_core      = { version = "0.8", default-features = false, features = ["embedded", "spawn_send"] }
hotaru_rt_embassy = "0.8"
hotaru_io_embedded = "0.8"
hcr_contract     = { path = "../hcr_contract", default-features = false }
hcr_sim          = { path = "../hcr_sim",      default-features = false }
```

```rust
// The std aliases (MQTT, DefaultMqttTransport, TokioRuntime) only exist under
// #[cfg(feature = "std")] — name the generics explicitly:
type Rt   = AppRuntime;                       // from define_runtime_worker_pool!
type Wire = hotaru_io_embedded::EmbeddedStream</* your socket */>;
type Ts   = hotaru_io_embedded::EmbeddedTransport</* … */>;
type Mqtt = MqttClientProtocol<Wire, Ts, Rt>;

hotaru_rt_embassy::define_runtime_worker_pool!(pub AppRuntime, worker_count = 3, job_queue_capacity = 8);

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    AppRuntime::init(spawner);
    // register MqttClientConfig under CLIENT_CONFIG_STATICS_KEY, then drive Mqtt via Client::run_wire
}
```

> **Use `spawn_send`, not `spawn_local`** — verified by building, 2026-07-31:
>
> | Feature combination | Result |
> | --- | --- |
> | `embedded,spawn_send` | ✅ compiles |
> | `embedded,spawn_local` | ❌ 70 errors |
> | `embedded,spawn_local_atomic` | ❌ not exposed by `hotaru_mqtt` |
>
> `hotaru_core::protocol::channel::Channel` is declared `Clone + Send + Sync + 'static`
> (`hotaru/hotaru_core/src/protocol/channel.rs:8`), but `spawn_local` is precisely the axis that swaps in
> `RefCell`/`Rc`, so `MqttChannel` cannot satisfy it. The upstream fix is to use the markers
> `hotaru_core` already provides (`marker.rs:113-145`):
>
> ```rust
> -pub trait Channel: Clone + Send + Sync + 'static {
> +pub trait Channel: Clone + MaybeSend + MaybeSync + 'static {
> ```
>
> That is a change to a core trait and belongs to the hotaru maintainers, not to this project. It is not
> blocking: `spawn_send` is fine on a single-core MCU (spin locks and atomics), and embassy runs `Send`
> tasks happily.

Required plumbing and constraints, all verified:

- Provide `impl From<<Ts as TransportSpec>::IoError> for MqttError` — the std build gets
  `From<std::io::Error>` for free, embedded targets must supply their own
  (`hotaru_mqtt/src/protocol.rs:132-142`).
- `hotaru_mqtt` is `#![cfg_attr(not(feature = "std"), no_std)]` and `#![forbid(unsafe_code)]`, using
  `async-channel` / `event-listener` / `OnceBox` instead of tokio primitives — this is precisely why the
  embassy port is possible (`hotaru_mqtt/src/lib.rs:19`).
- **The broker is std-only.** Firmware is always a client; never attempt to host a broker on-device.
- **arona never runs on-device.** It is std-only, and CAT is a server concern by design.
- Build with an explicit target, never `--workspace` (`cargo build -p hcr_firmware --target …`) —
  mixing std and no_std members trips hotaru's `compile_error!` guards.
- Tune `MqttSafety::with_max_packet_size` down (~8 KiB) to bound RAM; the 1 MiB server default is far too
  generous for an MCU.
- Use CBOR, not JSON, for `wireFormat`. A 500-command program is ~6 KB packed.

Firmware structure: subscribe `dn/cmd` and `dn/estop`; run the shared `hcr_sim` IR interpreter with servo
output instead of voxel removal; publish `up/telemetry` at 20–50 Hz QoS 0 through a bounded ring buffer that
drops oldest under backpressure; publish `up/state` retained on every status change; register a last will on
CONNECT.

## 7. Safety

**The simulator's head-collision geometry is not a safety system.** It is a deterministic geometric
constraint designed to make a teaching simulation well-behaved (`AGENTS.md`: collisions must stop at the last
safe pose and enter `error`, never be silently corrected). It assumes a perfectly known head pose, rigid
links, zero servo error, and no human movement. None of those hold in the physical world.

Anything that puts this arm near a real person requires, at minimum, and independent of software:

- a hardware e-stop that cuts servo power directly;
- mechanical limits that physically prevent the tool reaching the head volume;
- torque/current limiting, so a stall cannot press;
- a watchdog that de-energizes on loss of communication — noting that the LWT tells the *server* the device
  vanished, which does nothing for the person in the chair;
- no tool sharper than the demonstration requires.

The contract-level pieces (`dn/estop` on its own topic, hold-position on disconnect, `uncalibrated` as a
distinct device status) are necessary but **not sufficient**. They are interlocks against software faults,
not against the mechanism.

This is out of scope for the design and belongs in a hardware safety review before any human-proximate use.

## 8. Staging summary

| Tier | Hardware | Firmware | Speaks | Delivers |
| --- | --- | --- | --- | --- |
| 0 | ESP8266 kit as bought | unchanged | text over TCP | Physical arm driven from the simulator, now |
| 1 | ESP8266 kit | + PubSubClient | MQTT (text payloads) | Removes the translation hop |
| 2 | ESP32-S3 / C3 | Rust + embassy + `hotaru_mqtt` | MQTT (CBOR IR) | The hotaru embedded implementation; shares `hcr_sim` with the server |

Tier 0 is the only one that produces a working physical demo without new hardware or a firmware rewrite, and
nothing in it is wasted: the gateway's translation layer is what Tier 2 eventually retires.
