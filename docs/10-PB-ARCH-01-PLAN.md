# PB-ARCH-01 Implementation Plan — Typed MQTT Topic Grammar in `hcr_contract`

---

## 1. What we are building, and why it is first

**PB-ARCH-01** is one sentence in `docs/09-PAPER-BASELINE.md:148`: *"The device transport grammar is typed, not prose."* Today the whole of `01-CONTRACT.md` §4 (the thirteen-pattern topic tree, `docs/01-CONTRACT.md:139-157`) and §6.5 (the seven device payloads, `:280-286`) exist only as Markdown. `grep -rn "hcr/v1" crates/` returns exactly one hit, and it is a test comment asserting the absence (`crates/hcr_sim/tests/paper_baseline.rs:338`). `hcr_contract` has no `topic` module and no `device` module (`ls crates/hcr_contract/src` → `api.rs catalog.rs cutter.rs domain.rs lib.rs round.rs signature.rs wire.rs`).

The load-bearing consequence is the e-stop rule. `01-CONTRACT.md:291-292` says: *"**E-stop is a separate topic**, never queued behind `dn/cmd`. A stop that waits behind a program is not a stop."* Nothing enforces it. `Envelope::kind` is a free `String` (`crates/hcr_contract/src/wire.rs:45`) and `Envelope::reply_to` is a free `Option<String>` carrying a documented validation rule implemented nowhere (`wire.rs:52-55`, and `grep -rn reply_to crates/` matches only `wire.rs:55` and `wire.rs:72`). So `Envelope::new(id, "dev.estop.req", ts, payload)` published to a `format!`-built `dn/cmd` topic is, today, a perfectly ordinary line of Rust.

**Why everything downstream consumes it.** Four consumers are named in the chain, and every one of them either produces or classifies a topic string:

| Consumer | What it needs from PB-ARCH-01 |
| --- | --- |
| `ConnStream` adapter (`04-SERVICE.md:68-79`) | Nothing directly, but it is the seam where `Arc<str>` topics become typed values; the boundary needs one parse function, not thirteen `format!`s |
| Broker registration (`crates/hcr/src/deploy.rs:209-212`) | The §9 ACL grants (`01-CONTRACT.md:362-377`) as a predicate — `hotaru_mqtt` has none (see §6) |
| Device bridge (`04-SERVICE.md:190-199`) | Subscribes `hcr/v1/+/dev/+/up/#`, classifies five uplink leaves, republishes to `u/{uid}/evt/telemetry` — a total parser and a filter type |
| Firmware e-stop (`05-EMBEDDED.md:294-296`) | Two subscriptions (`dn/cmd`, `dn/estop`) and an allocation-free receive-path classifier that forces the author to name `Estop` separately |
| Deviation monitor (PB-HRI-01) | Escalates warn → pause → estop; must be structurally unable to escalate by pushing a stop into the command stream |

`docs/09-PAPER-BASELINE.md:242-245` reaches the same conclusion independently: *"Type the grammar in `hcr_contract` first… That is testable with no broker, no network and no hardware, it converts one Tier B test to Tier A on its own, and every later step — adapter, bridge, deviation monitor, firmware — consumes it."*

**What "done" means for Stage 1.** All five must hold:

1. `crates/hcr_contract/src/topic.rs` and `device.rs` exist; every one of the thirteen §4 patterns is constructible and renders byte-identically to the doc, and every one parses back to the value that rendered it.
2. `pb_arch_01_device_topic_grammar_is_typed` is no longer `#[ignore]`d, and asserts the separation through the public API (exact code in §5).
3. A `compile_fail` doctest in `device.rs`, executed by `cargo test -p hcr_contract`, shows that routing an e-stop payload through the command constructor is `error[E0308]`. This is the literal reading of the criterion's demand that the rule be *"checkable by the compiler"*.
4. A doc-conformance test scrapes the §4 fence out of `docs/01-CONTRACT.md` via `include_str!` and asserts bidirectional coverage of topics *and* of the QoS/retain columns — the same shape as `crates/hcr/src/hotaru_binding.rs:281-285`.
5. `cargo test --workspace --all-features` stays green at 8 ignored (was 9), and `cargo check -p hcr_contract --no-default-features` stays clean.

**What "done" explicitly does not mean:** preemption. PB-SAFE-03 stays red. See §7.

---

## 2. The chosen design

### 2.1 Winner and grafts

**`minimal-newtype` wins, 21/30.** (`enum-first` 20, `typestate` 14.)

It won on two of the three lenses — consumer-ergonomics (7.5) and no-std-firmware (8) — and lost the contract-fidelity lens (5.5) on things that are grafts, not architecture. Its spine is right: a flat borrowed `Copy` enum, one total parse, zero allocation on the device receive path, and the open segments (`{kind}`, `{service}`, `{method}`) left open because §4 never enumerates them.

Grafted in, as the judges directed:

| Graft | From | Why (judge) |
| --- | --- | --- |
| `Qos` as a two-variant enum with `const fn as_u8()` | enum-first | Makes §4's *"QoS 2 is deliberately unused"* (`01-CONTRACT.md:175`) unrepresentable instead of merely discouraged. `minimal-newtype`'s `qos() -> u8` re-admits it. (no-std judge, graft #5) |
| Addressed constructors for **all seven** device topics, not three | enum-first | `minimal-newtype` covered `estop`/`command`/`config` only, so `up/state` publishes were hand-rolled — "the exact untyped join between address and kind that the design's own rationale says it exists to delete." (ergonomics judge, SERIOUS) |
| `kind()` as a `const fn` **on the leaf enums** | typestate (idea), enum-first (placement) | One source for leaf + envelope kind. Replaces `minimal-newtype`'s split between `DevDown::as_str()` and `DeviceEstopReq::KIND`. (ergonomics judge, graft #7) |
| `TopicFilter` closed over the §9 grant shapes, with `covers()` | enum-first | The only construct that can express the criterion's real assertion — *a subscription covering `dn/cmd` does not cover `dn/estop`* — and the only home for §9's three ACL rules. `minimal-newtype` omitted filters on the false premise that "the broker already owns matching": `filter_matches` is private (`hotaru_mqtt/src/broker.rs:162`) and `hotaru_mqtt` performs **no** SUBSCRIBE/PUBLISH authorization at all. (fidelity judge; ergonomics judge, graft #3) |
| `DevDown::is_preemptive()` as `const fn` | enum-first | Carries the §6.5 obligation as data a firmware dispatcher can sort on, at one byte and zero monomorphizations — the same information `typestate` spends an entire `Lane` axis on. (no-std judge, graft #6) |
| `render_into(&mut dyn fmt::Write)` as the primary renderer | typestate | One impl drives a stack buffer on the MCU and a `String` on the server, with no second path to drift. (ergonomics + no-std judges) |
| `compile_fail` doctests as executed proof | typestate | rustdoc runs them under `cargo test` with no `trybuild` dev-dependency. `enum-first` left its five wrong calls as *comments*, which asserts nothing — for a criterion literally titled "checkable by the compiler". (all three judges) |

Two changes the judges demanded that are in **none** of the three designs as written:

- **Private fields on the publish bundle.** The contract-fidelity judge's `ALL THREE` fatal: `Publication`/`Publish` expose `pub topic`, `pub qos`, `pub retain`, `pub envelope`, so `let mut p = device::estop(..); p.topic = cmd_topic;` defeats every design's central claim in one line. Fixed here with private fields plus `topic()`/`qos()`/`retain()`/`envelope()`/`into_parts()`.
- **A working `dev/.../up/...` parse arm.** `minimal-newtype`'s is the only arm that bypasses `Seg::new` and, as written, does not typecheck (it mixes `Option` and `TopicResult` in one `.and_then`). Two judges flagged it. It is rewritten below to match its siblings.

One graft I am **declining**, with reason: `enum-first`'s `DeviceDownlink` / `DeviceUplink` `serde(untagged)` unions. Two judges grafted them, but the contract-fidelity judge's own finding against `enum-first` is that giving the two lanes a common Rust type lets a downstream `Vec<DeviceDownlink>` re-merge exactly what §6.5 separates. Seven named constructors get the coverage without reintroducing that. Adding the union later is additive if a consumer needs it.

### 2.2 Module placement, feature gating, re-exports

**Placement.** Two new modules, declared alphabetically in the existing single-line block (`lib.rs:16-22`):

```rust
pub mod api;
pub mod catalog;
pub mod cutter;
pub mod device;      // new
pub mod domain;
pub mod round;
pub mod signature;
pub mod topic;       // new
pub mod wire;
```

**Feature gating: none added.** `hcr_contract` has exactly one feature — `std`, default-on, whose only job is forwarding `std` to serde and serde_json (`crates/hcr_contract/Cargo.toml:15-17`). `alloc` is unconditional (`lib.rs:14`). The only `cfg` in the new code is `#[cfg(feature = "std")] impl std::error::Error for TopicError {}`, exactly as `crates/hcr_ws/src/error.rs:98-99` does it. No `thiserror` — no crate in this workspace uses it.

**One-way module dependency, stated as an invariant.** `topic.rs` must not reference `device.rs`. Rust links per item, so routing-only firmware that names `Topic` but never `DeviceCmdReq` never links `Program` and never pulls `f64` formatting into an image whose README says *"floating point is not pulled into the embedded image."* This is the cheapest available mitigation for the shared f64 flaw all three designs carry (§6).

**Re-exports** (`lib.rs:24-61` house style — one flat block per module, ASCII-alphabetized inside):

```rust
pub use device::{
    AxisConfig, AxisState, DeviceAckEvt, DeviceCfgReq, DeviceCmdReq, DeviceEstopReq,
    DeviceEventEvt, DeviceEventKind, DeviceOp, DeviceStateEvt, DeviceStatus,
    DeviceTelemetryEvt, Publish, WireFormat,
};
pub use topic::{
    DEVICE_UP_BRIDGE_FILTER, DevDown, DevUp, MAX_SEGMENT_LEN, MAX_TOPIC_LEN, MatchEvt,
    Qos, Seg, TOPIC_PREFIX, Topic, TopicError, TopicFilter, TopicResult,
    reply_to_is_own_inbox,
};
```

Deliberate deviation, worth a review comment: `signature.rs`'s free functions are re-exported flat (`lib.rs:60`) because their names are self-describing. `device.rs`'s constructors are not — bare `hcr_contract::state`, `::event`, `::ack` at the crate root would be ambiguous English. They stay module-qualified: `device::estop(…)`, `device::state(…)`. Only the types are re-exported.

### 2.3 `crates/hcr_contract/src/topic.rs`

```rust
//! Typed MQTT topic grammar.
//!
//! `docs/01-CONTRACT.md` §4 fixes thirteen topic patterns, their QoS and their
//! retain flag; §6.5 adds the rule that an e-stop travels on its own topic and
//! is never queued behind `dn/cmd`. Until this module existed the tree was a
//! Markdown table and every publisher spelled its topics by hand.
//!
//! # This module does not reference `crate::device`
//!
//! Rust links per item. Firmware that only routes an inbound topic depends on
//! `topic` alone, never instantiates [`crate::device::DeviceCmdReq`], never
//! links [`crate::domain::Program`], and so never pulls `f64` formatting into
//! an image that deliberately excludes floating point (`hcr-fw/README.md`).
//! Keep the dependency one-way.
//!
//! # What this enforces, and what it does not
//!
//! It enforces that a stop is addressed to its own topic and carries its own
//! envelope `kind`, and that a subscription covering `dn/cmd` does not cover
//! `dn/estop`. It does **not** enforce preemption: §8 forbids relying on
//! cross-topic ordering, so "handled ahead of ordinary commands" is a firmware
//! receive-path obligation (`PB-SAFE-03`), not a type-level one.

use core::fmt;

use alloc::string::String;

use crate::wire::PROTOCOL_VERSION;

/// Literal root of every topic: the `hcr` and `v1` segments of §4.
pub const TOPIC_PREFIX: &str = "hcr/v1";

// §1 puts the major version in both the topic tree and the envelope's `v`, so a
// major bump forks the whole tree. Re-literalling `v1` above is only legitimate
// while this holds.
const _: () = assert!(PROTOCOL_VERSION == 1);

/// Longest topic accepted, bytes. Well under MQTT's 65535 and under the 8 KiB
/// MCU packet cap (`05-EMBEDDED.md` §6), so a device never buffers a topic it
/// cannot answer.
pub const MAX_TOPIC_LEN: usize = 512;

/// Longest single variable segment, bytes.
pub const MAX_SEGMENT_LEN: usize = 128;

/// Segments after `hcr/v1/`. §4's longest pattern is five; `rpc` is four.
const MAX_SEGMENTS: usize = 5;

/// Filter the device bridge subscribes on the loopback hop (`04-SERVICE.md` §8).
pub const DEVICE_UP_BRIDGE_FILTER: &str = "hcr/v1/+/dev/+/up/#";

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

/// Why a topic could not be built or parsed.
///
/// The crate's first Rust error type. It follows the `hcr_ws` / `hcr_sim` shape
/// (`hcr_ws/src/error.rs:52-103`) — plain enum, `&'static str` reasons, manual
/// `Display`, std-gated `Error` — rather than adding a `thiserror` dependency
/// to a crate the firmware links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicError {
    /// A variable segment was empty, over-long, or held `/`, `+`, `#` or NUL.
    BadSegment(&'static str),
    /// The topic did not begin with `hcr/v1/`.
    WrongPrefix,
    /// Segment count or a literal segment matched no family in §4.
    NotInGrammar,
    /// A device or match leaf outside the enumerated set, e.g. `dn/reboot`.
    UnknownLeaf,
    /// Longer than [`MAX_TOPIC_LEN`].
    TooLong {
        /// Largest topic accepted.
        limit: usize,
    },
}

impl fmt::Display for TopicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSegment(reason) => write!(f, "invalid topic segment: {reason}"),
            Self::WrongPrefix => write!(f, "topic does not start with `{TOPIC_PREFIX}/`"),
            Self::NotInGrammar => write!(f, "topic matches no family in 01-CONTRACT.md §4"),
            Self::UnknownLeaf => write!(f, "unknown leaf segment"),
            Self::TooLong { limit } => write!(f, "topic exceeds {limit} bytes"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for TopicError {}

/// Result alias for this module.
pub type TopicResult<T> = Result<T, TopicError>;

// ---------------------------------------------------------------------------
// Variable segments
// ---------------------------------------------------------------------------

/// A validated variable topic level: `{tenant}`, `{deviceId}`, `{userId}`,
/// `{connId}`, `{matchId}`, `{service}`, `{method}`, `{kind}`.
///
/// The field is private, so a `Seg` cannot be forged from outside this module.
/// That is what makes a rendered topic un-forgeable: a hostile `deviceId`
/// cannot smuggle a `/` and address another principal's subtree, which in turn
/// is what makes [`reply_to_is_own_inbox`] a real check rather than a string
/// comparison.
///
/// §4 gives these segments no charset, so the rule enforced is MQTT's own
/// (no `+`, `#`, U+0000) plus a length bound. See the open questions in the
/// plan — a project-specific charset may be wanted later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seg<'a>(&'a str);

impl<'a> Seg<'a> {
    /// Validate `raw` as one topic level.
    pub fn new(raw: &'a str) -> TopicResult<Self> {
        if raw.is_empty() {
            return Err(TopicError::BadSegment("empty"));
        }
        if raw.len() > MAX_SEGMENT_LEN {
            return Err(TopicError::BadSegment("longer than MAX_SEGMENT_LEN bytes"));
        }
        for byte in raw.as_bytes() {
            match byte {
                b'/' => return Err(TopicError::BadSegment("contains `/`")),
                b'+' => return Err(TopicError::BadSegment("contains the `+` wildcard")),
                b'#' => return Err(TopicError::BadSegment("contains the `#` wildcard")),
                0 => return Err(TopicError::BadSegment("contains NUL")),
                _ => {}
            }
        }
        Ok(Self(raw))
    }

    /// The segment as written on the wire.
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

impl fmt::Display for Seg<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

// ---------------------------------------------------------------------------
// Delivery class
// ---------------------------------------------------------------------------

/// MQTT delivery guarantee.
///
/// §4 states QoS 2 is deliberately unused — *"its only advantage over QoS 1 is
/// exactly-once, which §8's idempotency keys already provide at lower cost"* —
/// so it has no variant and cannot be requested. Deliberately not `Serialize`:
/// QoS is an MQTT fixed-header field and never appears in a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Qos {
    /// Fire and forget. Telemetry only.
    AtMostOnce,
    /// At least once; duplicates handled by envelope `id` idempotency (§8).
    AtLeastOnce,
}

impl Qos {
    /// Wire value for an MQTT PUBLISH fixed header.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::AtMostOnce => 0,
            Self::AtLeastOnce => 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Literal leaves — one closed set per family
// ---------------------------------------------------------------------------

/// Leaf under `dev/{deviceId}/up/` — device to server. ACL grant (§9 rule 2):
/// a device may publish only under its own `up/` subtree, plus its `lwt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DevUp {
    /// 10–50 Hz joint samples.
    Telemetry,
    /// Retained status snapshot; declares `wireFormat`.
    State,
    /// Discrete events.
    Event,
    /// Command acknowledgements.
    Ack,
}

impl DevUp {
    /// Every leaf, in the order §4 lists them.
    pub const ALL: [DevUp; 4] = [DevUp::Telemetry, DevUp::State, DevUp::Event, DevUp::Ack];

    /// Stable wire spelling without relying on debug formatting.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telemetry => "telemetry",
            Self::State => "state",
            Self::Event => "event",
            Self::Ack => "ack",
        }
    }

    /// Envelope `kind` carried on this leaf (§6.5).
    ///
    /// The single source for the pairing: [`crate::device`]'s constructors read
    /// it rather than taking a `kind` argument, so an addressed message cannot
    /// claim a kind its topic does not carry.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Telemetry => "dev.telemetry.evt",
            Self::State => "dev.state.evt",
            Self::Event => "dev.event.evt",
            Self::Ack => "dev.ack.evt",
        }
    }

    /// QoS from the §4 policy table. Telemetry alone is 0: stale samples are
    /// worthless, so retransmission costs more than it is worth.
    pub const fn qos(self) -> Qos {
        match self {
            Self::Telemetry => Qos::AtMostOnce,
            Self::State | Self::Event | Self::Ack => Qos::AtLeastOnce,
        }
    }

    /// Retain flag from the §4 policy table.
    pub const fn retain(self) -> bool {
        match self {
            Self::State => true,
            Self::Telemetry | Self::Event | Self::Ack => false,
        }
    }

    const fn from_wire(raw: &str) -> Option<Self> {
        match raw.as_bytes() {
            b"telemetry" => Some(Self::Telemetry),
            b"state" => Some(Self::State),
            b"event" => Some(Self::Event),
            b"ack" => Some(Self::Ack),
            _ => None,
        }
    }
}

/// Leaf under `dev/{deviceId}/dn/` — server to device.
///
/// [`DevDown::Estop`] is a distinct value, not a flag on a command, because
/// §6.5 requires a stop never to queue behind `dn/cmd`. Every accessor below is
/// an exhaustive `match` rather than a `matches!` or a catch-all, so adding a
/// fourth downlink leaf fails the build until its kind, QoS, retain flag and
/// preemption status have all been decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DevDown {
    /// Ordinary commands. Queued.
    Cmd,
    /// Emergency stop, on its own topic.
    Estop,
    /// Retained configuration.
    Cfg,
}

impl DevDown {
    /// Every leaf, in the order §4 lists them.
    pub const ALL: [DevDown; 3] = [DevDown::Cmd, DevDown::Estop, DevDown::Cfg];

    /// Stable wire spelling without relying on debug formatting.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Estop => "estop",
            Self::Cfg => "cfg",
        }
    }

    /// Envelope `kind` carried on this leaf (§6.5).
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Cmd => "dev.cmd.req",
            Self::Estop => "dev.estop.req",
            Self::Cfg => "dev.cfg.req",
        }
    }

    /// QoS from the §4 policy table. Every downlink must arrive.
    pub const fn qos(self) -> Qos {
        match self {
            Self::Cmd | Self::Estop | Self::Cfg => Qos::AtLeastOnce,
        }
    }

    /// Retain flag from the §4 policy table.
    pub const fn retain(self) -> bool {
        match self {
            Self::Cfg => true,
            Self::Cmd | Self::Estop => false,
        }
    }

    /// Whether the receive path must service this leaf ahead of the others.
    ///
    /// §6.5's obligation as data a firmware dispatcher can sort on. The type
    /// system cannot make preemption happen (`PB-SAFE-03`); this makes the
    /// requirement impossible to forget when writing the dispatcher, because
    /// the enum is closed and every arm must be named.
    pub const fn is_preemptive(self) -> bool {
        match self {
            Self::Estop => true,
            Self::Cmd | Self::Cfg => false,
        }
    }

    const fn from_wire(raw: &str) -> Option<Self> {
        match raw.as_bytes() {
            b"cmd" => Some(Self::Cmd),
            b"estop" => Some(Self::Estop),
            b"cfg" => Some(Self::Cfg),
            _ => None,
        }
    }
}

/// Leaf under `match/{matchId}/evt/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchEvt {
    /// Phase and `closesAt`; retained so a late joiner learns the phase.
    State,
    /// Revealed at T0.
    Challenge,
    /// Final ranking.
    Results,
    /// Joins and leaves.
    Presence,
}

// `ALL`, `as_str`, `qos` (all AtLeastOnce), `retain` (State only) and
// `from_wire` follow `DevUp` exactly. `MatchEvt` has no `kind()`: §6.4's match
// payloads are not part of the §6.5 device family and are out of scope here.

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

/// One topic in the `hcr.v1` tree.
///
/// Flat on purpose: seven variants covering §4's thirteen patterns, with the
/// leaf enums collapsing the repetition. `Copy` and borrowed, so parsing on a
/// device receive path allocates nothing.
///
/// `{service}`, `{method}` and `{kind}` stay [`Seg`] rather than closed enums:
/// §4 enumerates no set for any of them, §1 mandates additive evolution
/// (*"new optional fields, new `kind` values, new topics"*), and closing them
/// would make a contract addition require a crate release before the topic is
/// even nameable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic<'a> {
    /// `hcr/v1/{tenant}/rpc/{service}/{method}` — client requests.
    Rpc {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Service name, e.g. `match`.
        service: Seg<'a>,
        /// Method name, e.g. `join`.
        method: Seg<'a>,
    },
    /// `hcr/v1/{tenant}/u/{userId}/rep/{connId}` — one connection's reply inbox.
    UserReply {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Owning user.
        user: Seg<'a>,
        /// Connection this inbox belongs to.
        conn: Seg<'a>,
    },
    /// `hcr/v1/{tenant}/u/{userId}/evt/{kind}` — server push to a user.
    UserEvent {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Target user.
        user: Seg<'a>,
        /// Event family, e.g. `telemetry` or `match`.
        kind: Seg<'a>,
    },
    /// `hcr/v1/{tenant}/dev/{deviceId}/up/{leaf}` — device to server.
    DeviceUp {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Device identity.
        device: Seg<'a>,
        /// Which uplink.
        leaf: DevUp,
    },
    /// `hcr/v1/{tenant}/dev/{deviceId}/dn/{leaf}` — server to device.
    DeviceDown {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Device identity.
        device: Seg<'a>,
        /// Which downlink.
        leaf: DevDown,
    },
    /// `hcr/v1/{tenant}/dev/{deviceId}/lwt` — last will.
    ///
    /// A sibling of `up`/`dn`, not a child (`01-CONTRACT.md:151`), so this is
    /// the tree's only five-segment device topic. §9 lists it separately from
    /// the `up/#` grant, which is why [`TopicFilter::DeviceUpSubtree`] does not
    /// cover it.
    DeviceLwt {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Device identity.
        device: Seg<'a>,
    },
    /// `hcr/v1/{tenant}/match/{matchId}/evt/{leaf}`.
    MatchEvent {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Match identity.
        match_id: Seg<'a>,
        /// Which event.
        leaf: MatchEvt,
    },
}

impl<'a> Topic<'a> {
    /// QoS this topic is published at, per the §4 policy table.
    pub const fn qos(&self) -> Qos {
        match self {
            Self::DeviceUp { leaf, .. } => leaf.qos(),
            Self::DeviceDown { leaf, .. } => leaf.qos(),
            Self::MatchEvent { leaf, .. } => leaf.qos(),
            Self::Rpc { .. }
            | Self::UserReply { .. }
            | Self::UserEvent { .. }
            | Self::DeviceLwt { .. } => Qos::AtLeastOnce,
        }
    }

    /// Retain flag, per the §4 policy table. Exactly four patterns retain:
    /// `up/state`, `dn/cfg`, `lwt` and `match/*/evt/state`.
    pub const fn retain(&self) -> bool {
        match self {
            Self::DeviceUp { leaf, .. } => leaf.retain(),
            Self::DeviceDown { leaf, .. } => leaf.retain(),
            Self::MatchEvent { leaf, .. } => leaf.retain(),
            Self::DeviceLwt { .. } => true,
            Self::Rpc { .. } | Self::UserReply { .. } | Self::UserEvent { .. } => false,
        }
    }

    /// Read a wire topic. Allocation-free: every segment borrows `topic`, and
    /// the split lands in a fixed stack array. This is the firmware's receive
    /// path, on a board whose free heap is the tracked scarcity metric and
    /// which has no `#[alloc_error_handler]`.
    pub fn parse(topic: &'a str) -> TopicResult<Self> {
        if topic.len() > MAX_TOPIC_LEN {
            return Err(TopicError::TooLong {
                limit: MAX_TOPIC_LEN,
            });
        }
        let rest = topic
            .strip_prefix(TOPIC_PREFIX)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or(TopicError::WrongPrefix)?;

        // Slice patterns after a split, as `hcr/src/binding.rs:211-224` routes HTTP.
        let mut buf = [""; MAX_SEGMENTS];
        let mut len = 0usize;
        for segment in rest.split('/') {
            if len == MAX_SEGMENTS {
                return Err(TopicError::NotInGrammar);
            }
            buf[len] = segment;
            len += 1;
        }

        Ok(match &buf[..len] {
            [tenant, "rpc", service, method] => Self::Rpc {
                tenant: Seg::new(tenant)?,
                service: Seg::new(service)?,
                method: Seg::new(method)?,
            },
            [tenant, "u", user, "rep", conn] => Self::UserReply {
                tenant: Seg::new(tenant)?,
                user: Seg::new(user)?,
                conn: Seg::new(conn)?,
            },
            [tenant, "u", user, "evt", kind] => Self::UserEvent {
                tenant: Seg::new(tenant)?,
                user: Seg::new(user)?,
                kind: Seg::new(kind)?,
            },
            [tenant, "dev", device, "up", raw] => Self::DeviceUp {
                tenant: Seg::new(tenant)?,
                device: Seg::new(device)?,
                leaf: DevUp::from_wire(raw).ok_or(TopicError::UnknownLeaf)?,
            },
            [tenant, "dev", device, "dn", raw] => Self::DeviceDown {
                tenant: Seg::new(tenant)?,
                device: Seg::new(device)?,
                leaf: DevDown::from_wire(raw).ok_or(TopicError::UnknownLeaf)?,
            },
            [tenant, "dev", device, "lwt"] => Self::DeviceLwt {
                tenant: Seg::new(tenant)?,
                device: Seg::new(device)?,
            },
            [tenant, "match", match_id, "evt", raw] => Self::MatchEvent {
                tenant: Seg::new(tenant)?,
                match_id: Seg::new(match_id)?,
                leaf: MatchEvt::from_wire(raw).ok_or(TopicError::UnknownLeaf)?,
            },
            _ => return Err(TopicError::NotInGrammar),
        })
    }

    /// Write the topic without allocating.
    ///
    /// The `dyn` receiver is deliberate: it keeps one rendering implementation
    /// for the MCU (writing into a fixed `ArrayString`) and the server, with no
    /// second code path to drift and no re-instantiation per writer type.
    pub fn render_into(&self, out: &mut dyn fmt::Write) -> fmt::Result {
        match self {
            Self::Rpc { tenant, service, method } => {
                write!(out, "{TOPIC_PREFIX}/{tenant}/rpc/{service}/{method}")
            }
            Self::UserReply { tenant, user, conn } => {
                write!(out, "{TOPIC_PREFIX}/{tenant}/u/{user}/rep/{conn}")
            }
            Self::UserEvent { tenant, user, kind } => {
                write!(out, "{TOPIC_PREFIX}/{tenant}/u/{user}/evt/{kind}")
            }
            Self::DeviceUp { tenant, device, leaf } => write!(
                out,
                "{TOPIC_PREFIX}/{tenant}/dev/{device}/up/{}",
                leaf.as_str()
            ),
            Self::DeviceDown { tenant, device, leaf } => write!(
                out,
                "{TOPIC_PREFIX}/{tenant}/dev/{device}/dn/{}",
                leaf.as_str()
            ),
            Self::DeviceLwt { tenant, device } => {
                write!(out, "{TOPIC_PREFIX}/{tenant}/dev/{device}/lwt")
            }
            Self::MatchEvent { tenant, match_id, leaf } => write!(
                out,
                "{TOPIC_PREFIX}/{tenant}/match/{match_id}/evt/{}",
                leaf.as_str()
            ),
        }
    }

    /// The wire topic as an owned string. The server path.
    pub fn render(&self) -> String {
        let mut buffer = String::new();
        // `String`'s `fmt::Write` is infallible.
        let _ = self.render_into(&mut buffer);
        buffer
    }

    /// Whether this topic is `user`'s own reply inbox in `tenant`.
    ///
    /// §5 makes it a MUST that the backend validate `replyTo` before honouring
    /// it, and §9 rule 3 repeats it. Nothing in the workspace checks it today.
    pub fn is_reply_inbox_of(&self, tenant: Seg<'_>, user: Seg<'_>) -> bool {
        matches!(
            self,
            Self::UserReply { tenant: t, user: u, .. } if *t == tenant && *u == user
        )
    }
}

impl fmt::Display for Topic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render_into(f)
    }
}

/// Whether `reply_to` names `user`'s own reply inbox in `tenant`.
///
/// The §5 check in the form the RPC handler has it: a raw string off
/// [`crate::Envelope::reply_to`], which stays a `String` on the wire.
pub fn reply_to_is_own_inbox(reply_to: &str, tenant: Seg<'_>, user: Seg<'_>) -> bool {
    Topic::parse(reply_to).is_ok_and(|topic| topic.is_reply_inbox_of(tenant, user))
}

// ---------------------------------------------------------------------------
// Filters — the §9 ACL grants, closed
// ---------------------------------------------------------------------------

/// A subscription filter, in exactly the shapes §9's ACL table admits.
///
/// Not a general MQTT matcher. `hotaru_mqtt` does not expose one — its
/// `filter_matches` is private (`hotaru_mqtt/src/broker.rs:162`) and it
/// performs no SUBSCRIBE/PUBLISH authorization at all — so §9's three rules
/// have no enforcement point upstream. This type supplies the *predicate*;
/// the *hook* is Stage 3 work and may need an upstream change.
///
/// There is deliberately no free-form constructor. A device subscribes
/// `dn/cmd` and `dn/estop` as two [`TopicFilter::Exact`] values, matching
/// `05-EMBEDDED.md:294`; the broader [`TopicFilter::DeviceDownSubtree`] exists
/// only because §9 grants it, and covering both lanes is exactly why the
/// firmware should not use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicFilter<'a> {
    /// Exactly one topic.
    Exact(Topic<'a>),
    /// `hcr/v1/{t}/rpc/#` — a browser's publish grant.
    RpcAll {
        /// Tenant scope.
        tenant: Seg<'a>,
    },
    /// `hcr/v1/{t}/u/{ownId}/#` — a browser's subscribe grant (§9 rule 1).
    UserSubtree {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Owning user.
        user: Seg<'a>,
    },
    /// `hcr/v1/{t}/dev/{ownId}/up/#` — a device's publish grant (§9 rule 2).
    ///
    /// Deliberately does not cover [`Topic::DeviceLwt`]: §9 lists `…/lwt`
    /// separately from the `up/#` subtree, so the will is registered as an
    /// [`TopicFilter::Exact`].
    DeviceUpSubtree {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Device identity.
        device: Seg<'a>,
    },
    /// `hcr/v1/{t}/dev/{ownId}/dn/#` — a device's subscribe grant.
    DeviceDownSubtree {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Device identity.
        device: Seg<'a>,
    },
    /// `hcr/v1/+/dev/+/up/#` — the loopback bridge (`04-SERVICE.md` §8).
    /// `None` renders `+`.
    DeviceUpAll {
        /// Tenant, or all tenants.
        tenant: Option<Seg<'a>>,
        /// Device, or all devices.
        device: Option<Seg<'a>>,
    },
    /// `hcr/v1/{t}/match/{matchId}/evt/#` — membership-scoped
    /// (`06-MULTIPLAYER.md` §6).
    MatchEvents {
        /// Tenant scope.
        tenant: Seg<'a>,
        /// Match identity.
        match_id: Seg<'a>,
    },
}

impl<'a> TopicFilter<'a> {
    /// Whether this filter covers `topic`.
    ///
    /// Matched over the [`Topic`] ADT, not over strings, so there is no
    /// wildcard parser to get wrong and the answer is total.
    pub fn covers(&self, topic: &Topic<'_>) -> bool {
        match (self, topic) {
            (Self::Exact(mine), other) => mine == other,
            (Self::RpcAll { tenant }, Topic::Rpc { tenant: t, .. }) => tenant == t,
            (Self::UserSubtree { tenant, user }, Topic::UserReply { tenant: t, user: u, .. })
            | (Self::UserSubtree { tenant, user }, Topic::UserEvent { tenant: t, user: u, .. }) => {
                tenant == t && user == u
            }
            (
                Self::DeviceUpSubtree { tenant, device },
                Topic::DeviceUp { tenant: t, device: d, .. },
            ) => tenant == t && device == d,
            (
                Self::DeviceDownSubtree { tenant, device },
                Topic::DeviceDown { tenant: t, device: d, .. },
            ) => tenant == t && device == d,
            (Self::DeviceUpAll { tenant, device }, Topic::DeviceUp { tenant: t, device: d, .. }) => {
                tenant.is_none_or(|s| s == *t) && device.is_none_or(|s| s == *d)
            }
            (
                Self::MatchEvents { tenant, match_id },
                Topic::MatchEvent { tenant: t, match_id: m, .. },
            ) => tenant == t && match_id == m,
            _ => false,
        }
    }

    /// Write the filter without allocating.
    pub fn render_into(&self, out: &mut dyn fmt::Write) -> fmt::Result { /* one arm per variant */ }
}

impl fmt::Display for TopicFilter<'_> { /* delegates to render_into */ }
```

### 2.4 `crates/hcr_contract/src/device.rs`

```rust
//! Device payloads and the addressed-publish constructors.
//!
//! Mirrors `docs/schema/hcr-v1.d.ts` §6 (`DeviceState`, `DeviceCommand`, …).
//! Where the Rust mirror `docs/schema/hcr_v1.rs` disagrees — it adds a sixth
//! `op: "text"` the TypeScript union lacks — the TypeScript file wins, per
//! `lib.rs:4-6`, and `textCmd` rides an existing `op`. Two further tie-breaks,
//! both flagged for the contract owner: `dev.cfg.req` carries `{ axes }` only
//! (schema `hcr-v1.d.ts:819`), not the prose's `speedLimits`/`calibration`;
//! and `AxisConfig.jointId` is required-but-nullable, following the TS form.
//!
//! # Why a stop cannot be published as a command
//!
//! §6.5 says e-stop travels on its own topic, never queued behind `dn/cmd`.
//! Enforced here by *not offering* a call that could break it: the seven
//! constructors below are the only way to build an addressed device message,
//! each hard-codes its leaf, and each returns a [`Publish`] whose fields are
//! private. There is no public function taking a topic and a payload
//! separately, so there is no call site at which the pairing can be got wrong.

use alloc::string::{String, ToString};

use serde::{Deserialize, Serialize};

use crate::domain::Program;
use crate::topic::{DevDown, DevUp, Qos, Seg, Topic, TopicError, TopicResult};
use crate::wire::{Envelope, HcrError};

/// A message together with where and how it must be published.
///
/// Fields are private. An adapter takes the whole value and reads it through
/// the accessors, so the topic, the delivery policy and the envelope cannot be
/// separated and re-paired after construction — which is the one hole every
/// candidate design left open.
#[derive(Debug, Clone)]
pub struct Publish<'a, P> {
    topic: Topic<'a>,
    qos: Qos,
    retain: bool,
    envelope: Envelope<P>,
}

impl<'a, P> Publish<'a, P> {
    /// Where it goes.
    pub const fn topic(&self) -> Topic<'a> {
        self.topic
    }

    /// QoS, from the §4 policy table.
    pub const fn qos(&self) -> Qos {
        self.qos
    }

    /// Retain flag, from the §4 policy table.
    pub const fn retain(&self) -> bool {
        self.retain
    }

    /// What goes there.
    pub const fn envelope(&self) -> &Envelope<P> {
        &self.envelope
    }

    /// Consume into the four values a broker client needs.
    pub fn into_parts(self) -> (Topic<'a>, Qos, bool, Envelope<P>) {
        (self.topic, self.qos, self.retain, self.envelope)
    }
}

/// `dev.estop.req` — emergency stop. Payload is one sentence of reason.
///
/// A distinct type from [`DeviceCmdReq`] on purpose: that is what makes the
/// mix-up a type error rather than a runtime one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEstopReq {
    /// Why the stop was issued; logged, never parsed.
    pub reason: String,
}

/// Operation a `dev.cmd.req` carries.
///
/// There is deliberately no `Estop` variant: a stop is not an op, it is a
/// different topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceOp {
    /// Run `program`.
    Run,
    /// Return to the home pose.
    Home,
    /// Stop at the end of the current motion. **Not** an e-stop.
    Stop,
    /// Resume a paused program.
    Resume,
    /// Report state.
    Query,
}

/// `dev.cmd.req` — an ordinary command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCmdReq {
    /// `id` of the request this command realises.
    pub corr: String,
    /// What to do.
    pub op: DeviceOp,
    /// Present for [`DeviceOp::Run`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<Program>,
    /// Pre-translated text for kit firmware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_cmd: Option<String>,
}

// `DeviceStateEvt`, `DeviceTelemetryEvt`, `DeviceEventEvt`, `DeviceEventKind`,
// `DeviceAckEvt`, `DeviceCfgReq`, `AxisState`, `AxisConfig`, `DeviceStatus` and
// `WireFormat` follow the same shape, field-for-field from `hcr-v1.d.ts:645-702`
// and `:819`. Note `DeviceStateEvt` carries the two fields the §6.5 prose omits:
// required `queuedCommands: u32` and optional `fault: Option<HcrError>`.

// ---------------------------------------------------------------------------
// Addressed constructors — the only route from a payload onto the wire
// ---------------------------------------------------------------------------

/// Address an emergency stop.
///
/// The only constructor of an e-stop message. It fixes the leaf to
/// [`DevDown::Estop`], so a stop cannot be issued on `dn/cmd`.
///
/// ```
/// # use hcr_contract::{device, topic::{Qos, Seg}};
/// let tenant = Seg::new("acme").unwrap();
/// let arm = Seg::new("arm-7").unwrap();
/// let stop = device::estop(tenant, arm, "01J".into(), 0, "operator pressed stop".into());
/// assert_eq!(stop.topic().render(), "hcr/v1/acme/dev/arm-7/dn/estop");
/// assert_eq!(stop.envelope().kind, "dev.estop.req");
/// assert_eq!(stop.qos(), Qos::AtLeastOnce);
/// assert!(!stop.retain());
/// ```
///
/// A stop payload cannot be routed through the command path:
///
/// ```compile_fail
/// # use hcr_contract::{device::{self, DeviceEstopReq}, topic::Seg};
/// # let t = Seg::new("acme").unwrap();
/// # let d = Seg::new("arm-7").unwrap();
/// // error[E0308]: mismatched types
/// //   expected `DeviceCmdReq`, found `DeviceEstopReq`
/// device::command(t, d, "01J".into(), 0, DeviceEstopReq { reason: "stop".into() });
/// ```
pub fn estop<'a>(
    tenant: Seg<'a>,
    device: Seg<'a>,
    id: String,
    ts: u64,
    reason: String,
) -> Publish<'a, DeviceEstopReq> {
    downlink(tenant, device, DevDown::Estop, id, ts, DeviceEstopReq { reason })
}

/// Address an ordinary command.
pub fn command<'a>(
    tenant: Seg<'a>,
    device: Seg<'a>,
    id: String,
    ts: u64,
    cmd: DeviceCmdReq,
) -> Publish<'a, DeviceCmdReq> {
    downlink(tenant, device, DevDown::Cmd, id, ts, cmd)
}

/// Address a retained configuration push.
pub fn config<'a>(/* … DevDown::Cfg … */) -> Publish<'a, DeviceCfgReq> { /* … */ }

/// Address a retained state snapshot. Also the body of the last will —
/// see [`will`].
pub fn state<'a>(/* … DevUp::State … */) -> Publish<'a, DeviceStateEvt> { /* … */ }

/// Address a telemetry sample. QoS 0, never retained.
pub fn telemetry<'a>(/* … DevUp::Telemetry … */) -> Publish<'a, DeviceTelemetryEvt> { /* … */ }

/// Address a discrete device event.
pub fn event<'a>(/* … DevUp::Event … */) -> Publish<'a, DeviceEventEvt> { /* … */ }

/// Address a command acknowledgement.
pub fn ack<'a>(/* … DevUp::Ack … */) -> Publish<'a, DeviceAckEvt> { /* … */ }

/// Address the last-will message registered at CONNECT.
///
/// **Inferred, not specified.** `HcrMessages` declares no `dev.lwt.*` kind and
/// §4 gives the topic only a QoS/retain policy. `05-EMBEDDED.md:209` has the
/// broker *"publish the retained offline state on the device's behalf"*, so
/// this types the will as a retained `DeviceStateEvt` with `dev.state.evt`.
/// That leaves two retained `DeviceStateEvt`s — `up/state` and `lwt` — with no
/// stated precedence rule. See the plan's open questions.
pub fn will<'a>(
    tenant: Seg<'a>,
    device: Seg<'a>,
    id: String,
    ts: u64,
    offline: DeviceStateEvt,
) -> Publish<'a, DeviceStateEvt> {
    let topic = Topic::DeviceLwt { tenant, device };
    Publish {
        qos: topic.qos(),
        retain: topic.retain(),
        topic,
        envelope: Envelope::new(id, DevUp::State.kind().to_string(), ts, offline),
    }
}

/// Shared body of the three downlink constructors. Private: the leaf must not
/// be a caller-supplied parameter.
fn downlink<'a, P>(
    tenant: Seg<'a>,
    device: Seg<'a>,
    leaf: DevDown,
    id: String,
    ts: u64,
    payload: P,
) -> Publish<'a, P> {
    let topic = Topic::DeviceDown { tenant, device, leaf };
    Publish {
        qos: topic.qos(),
        retain: topic.retain(),
        // `kind` comes from the leaf, never from the caller.
        envelope: Envelope::new(id, leaf.kind().to_string(), ts, payload),
        topic,
    }
}

/// Shared body of the four uplink constructors. Private, same reason.
fn uplink<'a, P>(/* … DevUp … */) -> Publish<'a, P> { /* … */ }
```

### 2.5 The firmware receive path this buys

```rust
// hcr-fw, no_std, no allocation on this path.
fn on_message(raw_topic: &str, payload: &[u8], arm: &mut Arm) {
    // §1: receivers MUST NOT fail on an unknown kind — log and drop.
    let Ok(topic) = Topic::parse(raw_topic) else { return };
    match topic {
        // §6.5: handled ahead of ordinary commands, in the receive path itself.
        Topic::DeviceDown { leaf: DevDown::Estop, .. } => arm.emergency_stop(),
        Topic::DeviceDown { leaf: DevDown::Cmd, .. } => arm.enqueue(payload),
        Topic::DeviceDown { leaf: DevDown::Cfg, .. } => arm.apply_config(payload),
        _ => {} // not ours
    }
}
```

The match is exhaustive over `DevDown`, so a fourth downlink leaf added to the contract is a compile error here rather than silent traffic loss. `parse` performs one `strip_prefix`, at most five `split` steps into a stack array, a slice-pattern match and a `&str` compare — no `String`, no `Vec`, no `format!`, no heap traffic in the path that has to stop the arm. That matters concretely: `hcr-fw` has no `#[alloc_error_handler]`, and its panic handler feeds the watchdog forever with servos energized (`hcr-fw/hcr-gateway/src/image/panic.rs:5-13`), so an allocation failure in front of the stop is a fail-dangerous outcome.

---

## 3. Why not the others

### `typestate` — 14/30. Killed by having no total parser.

`Topic<D, L>::parse` takes the leaf as a *type parameter*, so there is no call that turns an arbitrary wire topic into a value. The consumer-ergonomics judge called this fatal and was right about why: the device bridge subscribes `hcr/v1/+/dev/+/up/#` and sees five possible leaves, so it must write `if let Ok(t) = Topic::<Up, Telemetry>::parse(&raw) { … } else if let Ok(t) = Topic::<Up, State>::parse(&raw) { … }` — N full re-splits per message at 20–50 Hz, N bodies that cannot share code because `t` has a different type in each arm, and, decisively, **no compile error when the contract gains a fourteenth topic**, because a cascade of `if let Ok` is not a match. The one design advertising compile-time totality is the only one that loses it at the boundary where the contract actually changes.

The no-std judge found the same defect costs heap: each `parse::<L>` attempt allocates two `Vec<&str>`, one of them rebuilt from a compile-time-constant `L::PATH` on every call, which makes identifying an e-stop the most expensive operation in the module on the one target where it is safety-critical. And the contract-fidelity judge found `Leaf::Payload` — one associated type per leaf — structurally wrong for three of §4's four families: `u/{userId}/rep/{connId}` carries every `*.res` payload *plus* `HcrError` under kind `error` (§7, `01-CONTRACT.md:308`), and `rpc/{service}/{method}`'s kind is a function of the variable segments, not a const. That is why three families are "elided in a comment" rather than written: the design cannot express them, so it types roughly 8 of 13 patterns.

Its headline feature, the `Lane` axis, does not reach the party at risk. It is delivered to firmware only as a bound on `CommandQueue`, which the design itself tells the ESP8266 to reimplement — so the guarantee holds on the server, which was never going to queue a stop behind a program, and evaporates on the MCU, which was. Meanwhile the failure §6.5 actually names — `fn on_estop(&mut self, e) { self.pending_stop = Some(e); }` drained after the command queue — type-checks perfectly.

`Lane`'s *idea* survives here as `DevDown::is_preemptive()`: same information, one byte, zero monomorphizations.

### `enum-first` — 20/30. Lost by one point, on enforcement that is convention rather than typing.

It is the best raw transcription of §4 and it contributed the most grafted material. What killed it is that its central claim — *"the only public way to address a downlink"* — is false as written. `Publication` derives `Debug, Clone` with all-pub fields and no private member, so `Publication { topic: cmd_topic, qos: 1, retain: false, envelope: estop_envelope }` compiles verbatim outside the crate. `Topic::device_down(t, d, DeviceDown::Cmd)` is public and takes the leaf as a parameter, contradicting *"the leaf is not an argument."* And `Topic` variant fields are `pub String`, so `Topic::Rpc { tenant: "victim-tenant".into(), .. }` renders a well-formed topic in another tenant's subtree with no validation — precisely the §9 rule-1/rule-2 escape the constructors exist to prevent.

Two further overclaims the fidelity judge caught: `qos()`'s `Self::Device { .. }` catch-all and `retain()`'s `matches!` silently give a new leaf QoS 1 / retain false, so *"adding a leaf fails the build until its QoS and retain flag are decided"* is untrue; and it writes its five wrong calls as **comments** rather than tests, for a criterion titled "checkable by the compiler."

Its fatal fidelity flaw is `UserEvent`, a closed four-variant enum over `{kind}` — a segment §4 never enumerates. Two of the four (`Submission`, `Session`) appear in no document, so it simultaneously invents contract and makes legal topics unrepresentable: `submission.result.evt` is the main server push and has no documented topic at all, so whatever `{kind}` it lands on parses as `UnknownLeaf`. §1 mandates new topics as additive minor evolution; a closed enum turns a contract addition into a crate release before the topic is nameable.

The no-std judge found its advertised allocation-free receive path — *"borrows out of `topic` instead of building a `Topic`"* — opens with `let segments: Vec<&str> = topic.split('/').collect();`. And the ergonomics judge flagged two hand-maintained parse paths (`Topic::parse` and `classify_device_topic`) in a design whose stated enemy is a second copy of a normative table.

None of this is architectural. All of it is repairable, which is why the fixes are grafts and the spine is `minimal-newtype`'s.

---

## 4. Staged work plan

### Stage 1 — PB-ARCH-01 alone. Independently shippable. **This is the only stage that is not speculative.**

| Item | Detail |
| --- | --- |
| **New** | `crates/hcr_contract/src/topic.rs` — ~600 lines body + ~250 inline tests |
| **New** | `crates/hcr_contract/src/device.rs` — ~380 lines body + ~200 inline tests |
| **Edit** | `crates/hcr_contract/src/lib.rs` — 2 `pub mod` lines at :16-22, 2 `pub use` blocks at :24-61 (~22 lines) |
| **Edit** | `crates/hcr_sim/tests/paper_baseline.rs` — rewrite `pb_arch_01_device_topic_grammar_is_typed` in place at file top level, **not** inside the `#[cfg(feature = "planner")] mod tier_a` at :49-50 (a topic test needs no planner; putting it there hides it from the default run, which compiles 9 tests today). Fix the `§5` → `§4` citation at :336. ~50 lines net |
| **Edit** | `docs/09-PAPER-BASELINE.md` — fix `§5, §6.5` at :220 and `§5–6.5` at :148; move PB-ARCH-01 from the Tier B table to the coverage table |
| **Edit** | `Cargo.toml` — delete the stale comment block at :11-17 (all four claims are false; see §6) |
| **Edit, optional** | The four stale `docs/backend/…` paths: `hcr_contract/src/lib.rs:5`, `hcr_contract/src/wire.rs:5`, `hcr_ws/src/lib.rs:5`, `Cargo.toml:17` |

**Tests that prove it:**

1. `every_documented_topic_renders_exactly` — one `assert_eq!` per §4 pattern against the literal, including the five-segment `lwt`.
2. `parsing_a_rendered_topic_returns_the_same_value` — over all thirteen, driven from the `ALL` tables.
3. `qos_and_retain_match_the_policy_table` — exactly four retain, exactly one is `AtMostOnce`, and `Qos` has no third variant.
4. `a_segment_may_not_smuggle_a_level` — `Seg::new("a/b")`, `"+"`, `"#"`, `""`, 129 bytes → each expected `TopicError`.
5. `a_topic_outside_the_grammar_is_rejected` — wrong prefix, `hcr/v2/…`, six segments, `dn/reboot`.
6. `a_reply_inbox_belongs_to_exactly_one_user` — §5's MUST, including the prefix-lookalike `u/u10/…` against user `u1` and the trailing-segment form.
7. `a_command_subscription_does_not_cover_the_stop` — `TopicFilter::Exact(cmd).covers(&estop) == false`; `DeviceUpAll` covers `up/telemetry` but not `dn/cmd` and not `lwt`.
8. `device_payloads_serialize_at_the_documented_shape` — exact JSON strings per the house convention (`cutter.rs:924-928`), plus `WireFormat` as `"json"|"cbor"|"text"`, `DeviceStatus` including `"uncalibrated"`, and a `DeviceStateEvt` without `rssi`/`fault` still parsing.
9. `a_stop_payload_cannot_be_published_as_a_command` — the `compile_fail` doctest in §2.4, executed by `cargo test -p hcr_contract` with no `trybuild` dependency.
10. `every_documented_topic_is_produced_by_a_constructor` — `#[cfg(all(test, feature = "std"))]`, `include_str!("../../../docs/01-CONTRACT.md")`, scrape the `hcr/v1/…` lines out of the §4 fence, substitute fixed values for `{…}` placeholders, and assert bidirectional coverage of topics **and** of the QoS/retain columns. This is the single highest-value test in the PR: it is what stops the transcribed policy tables from silently disagreeing with the Markdown they were copied from. Precedent: `crates/hcr/src/hotaru_binding.rs:281-285`.
11. The graduated `pb_arch_01_device_topic_grammar_is_typed` (§5).

**Gates:** `cargo test --workspace --all-features` green at 313 defined / 8 ignored; `cargo check -p hcr_contract --no-default-features` clean; `missing_docs` clean (every pub item, field and variant needs a `///`).

**Unblocks:** all of Stages 2–6. Ships value alone even if the chain stalls: it converts one Tier B criterion to Tier A, and it gives the frontend and the gateway a typed route table today.

---

### Stage 2 — `ConnStream` adapter. **Blocked; see §6 before scheduling.**

Build `WsStream<W> / WsReadHalf / WsWriteHalf` per `04-SERVICE.md:68-79`, wrapping `hcr_ws`'s sans-io `Decoder` + `encode_server_frame`. Estimated 600–900 LOC plus a conformance suite (`04-SERVICE.md:106`), in a new crate — `hcr_ws` excludes it deliberately (`hcr_ws/src/lib.rs:17-20`) and that exclusion is correct.

Known unsolved sub-problems the ws-crate survey established: `WsSession` cannot be split, and auto-pongs generated on the read path land in the write path's buffer (`session.rs:60-62`), so the adapter must either share one session behind a lock or compose `Decoder` + `encode_server_frame` with an explicit control-frame channel. There is no `read_into(&mut [u8]) -> usize` — only `inbound()` (borrow-all) and `take_inbound()` (move-all) — so the adapter maintains its own leftover buffer and cursor. `close_sent` is private and `write` silently no-ops after close, so the adapter must track close state itself. `WsError` is non-latching: a header-level failure leaves the offending bytes buffered and every subsequent `poll()` returns the same error forever, so any `WsError` must be treated as terminal.

Consumes from Stage 1: nothing directly. It is the seam.

**Do not start this stage before resolving the hotaru version blocker in §6.**

### Stage 3 — Broker instantiation and protocol registration. *Speculative from here.*

`crates/hcr/src/deploy.rs:209-212` currently calls `.single_protocol(ProtocolBuilder::new(HTTP::server(safety)))`, which **replaces** the whole registry rather than adding to it. Multi-protocol needs `.handle(HandlerBuilder::new().protocol(a).protocol(b))`, with the framework sniffing the first byte (`MqttProtocol::detect` is `(b >> 4) == 1`). One `App` carries one binding, so `:8883` MQTTS alongside `:443` needs a second `Server` instance.

Consumes from Stage 1: `TopicFilter::covers` as the ACL predicate. But `hotaru_mqtt` has **no** authorization hook to attach it to — `Authenticator` returns only accept/reject and fires once at CONNECT; SUBSCRIBE and PUBLISH are unauthorized; any authenticated client may subscribe `#`. It also has **no retained-message store** (fanout forces `retain: false`, `broker.rs:511-518`) and **no persistent sessions**. Four of our thirteen topics retain, and retain is load-bearing for `up/state` and for the LWT. This is unimplemented upstream work, not configuration, and it is the largest unknown in the chain.

### Stage 4 — Device bridge. *Speculative.*

Loopback MQTT client subscribing `DEVICE_UP_BRIDGE_FILTER`, downsampling telemetry to ≤10 Hz and republishing to `u/{uid}/evt/telemetry` (`04-SERVICE.md:190-199`). Consumes `Topic::parse` and the seven addressed constructors directly. Two contradictions must be settled first: `04-SERVICE.md` §8 specifies no QoS for the republish while §4 assigns QoS 1 to every `u/*/evt/{kind}`, which silently promotes telemetry against §4's own rationale; and §8 never says how the bridge authenticates against a fail-closed broker.

### Stage 5 — Firmware e-stop and comms watchdog (PB-SAFE-03, PB-SAFE-04). *Speculative.*

Not an MQTT task. `hcr-fw` is a pure inbound HTTP server; `acquire_channel` is hard-coded `Unsupported`, and every local `hotaru_mqtt` copy is std-and-tokio-only. So the near-term shape is a **state-machine transplant plus a new deadman timer**, with `dn/estop` arriving via the gateway translation.

The transplant source is `hotaru-esp8266/rust_core/src/state.rs`, which already has the whole layer: `ArmState { armed, emergency_stopped, current, targets, … }` (:54-62), `RouteAction::{Enable, Disarm, EmergencyStop}` (:20-28), `emergency_stop()` which de-energizes *first* then latches and collapses targets onto current (:222-235), the `!armed || emergency_stopped` gate with distinct 409 codes (:153-174), and `motion_tick` enforcing 1° per 20 ms (:17-18, :239-288). Its panic and alloc-error handlers both `servo_detach_all()` before spinning and deliberately do **not** feed the watchdog (`lib.rs:49-58`, `ffi_alloc.rs:59-67`) — the opposite of `hcr-fw`'s current behaviour.

Net-new on the `hcr-fw` side: `fn detach(&mut self)` on the `ServoHandle` trait (`hcr-platform/src/traits/servo.rs:5-13` has no such method, though the C ABI's `hcr_servo_detach` already exists), a rate-limit unit conversion from whole degrees to tenths, and the comms watchdog — which **neither** codebase implements. `Gateway::poll` (`hcr-gateway/src/gateway.rs:123-130`) already reads `millis()` every iteration and is the natural home for both the deadman check and the motion tick.

Consumes from Stage 1: `DevDown::is_preemptive()` and the exhaustive `Topic::parse` match as the dispatcher's shape.

### Stage 6 — Deviation monitor (PB-HRI-01). *Speculative and least resolved.*

Subscribe planned and executed trajectories, compute deviation, publish warn → pause → estop with hysteresis (`paper_baseline.rs:184-203`). Consumes `device::estop` as its escalation terminal. Nothing in the surveys establishes what "planned trajectory" is on the wire, what the deviation metric is, or what the hysteresis thresholds are. Treat this stage as an outline, not a plan.

---

## 5. How PB-ARCH-01 graduates

Rewritten in place in `crates/hcr_sim/tests/paper_baseline.rs`, at **file top level** — the file header at :20 requires *"being rewritten against the API that closed it — never by being deleted"*, and `mod tier_a` is `#[cfg(feature = "planner")]`, which would hide a planner-free test from the default run.

```rust
/// **PB-ARCH-01** — the device topic grammar is typed, not prose.
///
/// `docs/01-CONTRACT.md` §4 specifies the topic tree and §6.5 the device
/// payloads, including the rule that separates e-stop from commands. That rule
/// is now a type: `hcr_contract::device` has one addressed constructor per
/// device topic, each hard-coding its leaf and deriving the envelope `kind`
/// from it, and `hcr_contract::topic::TopicFilter` can answer whether one
/// subscription covers another topic.
///
/// What this does **not** prove is preemption. §8 forbids relying on
/// cross-topic ordering, so "firmware must handle it in its receive path ahead
/// of ordinary commands" stays a scheduling obligation — `PB-SAFE-03`, still
/// Tier B. The compile-time half of the criterion (a stop payload cannot be
/// handed to the command constructor) is a `compile_fail` doctest on
/// `hcr_contract::device::estop`; this test asserts the runtime-observable half.
#[test]
fn pb_arch_01_device_topic_grammar_is_typed() {
    use hcr_contract::device::{self, DeviceCmdReq, DeviceOp};
    use hcr_contract::topic::{DevDown, DevUp, Qos, Seg, Topic, TopicFilter};

    let tenant = Seg::new("acme").expect("a legal tenant segment");
    let arm = Seg::new("arm-7").expect("a legal device segment");

    let stop = device::estop(
        tenant,
        arm,
        "01JBQ0STOP".into(),
        1_700_000_000_000,
        "operator pressed stop".into(),
    );
    let cmd = device::command(
        tenant,
        arm,
        "01JBQ0CMD".into(),
        1_700_000_000_000,
        DeviceCmdReq {
            corr: "01JBQ0REQ".into(),
            op: DeviceOp::Home,
            program: None,
            text_cmd: None,
        },
    );

    // 1. A stop and a command are different topics, rendered exactly as §4
    //    writes them. The leaf is not a parameter of either constructor.
    assert_eq!(stop.topic().render(), "hcr/v1/acme/dev/arm-7/dn/estop");
    assert_eq!(cmd.topic().render(), "hcr/v1/acme/dev/arm-7/dn/cmd");
    assert_ne!(stop.topic(), cmd.topic());

    // 2. The envelope `kind` is derived from the leaf, never supplied by the
    //    caller — so an e-stop cannot claim to be `dev.cmd.req`.
    assert_eq!(stop.envelope().kind, "dev.estop.req");
    assert_eq!(cmd.envelope().kind, "dev.cmd.req");
    assert_eq!(DevDown::Estop.kind(), stop.envelope().kind);
    assert_eq!(DevDown::Cmd.kind(), cmd.envelope().kind);

    // 3. §4's QoS/retain policy travels with the topic rather than with the
    //    call site, so a publisher cannot get it wrong by passing an argument.
    assert_eq!(stop.qos(), Qos::AtLeastOnce);
    assert!(!stop.retain());
    let telemetry = Topic::DeviceUp { tenant, device: arm, leaf: DevUp::Telemetry };
    let state = Topic::DeviceUp { tenant, device: arm, leaf: DevUp::State };
    assert_eq!(telemetry.qos(), Qos::AtMostOnce);
    assert!(!telemetry.retain());
    assert!(state.retain());

    // 4. THE RULE, as a containment fact rather than a string comparison:
    //    a subscription covering `dn/cmd` does not cover `dn/estop`.
    let cmd_only = TopicFilter::Exact(cmd.topic());
    assert!(cmd_only.covers(&cmd.topic()));
    assert!(
        !cmd_only.covers(&stop.topic()),
        "a stop must never be delivered through the command subscription"
    );

    // 5. The device's whole §9 downlink grant does cover both, which is
    //    precisely why 05-EMBEDDED.md §6 requires two subscriptions rather
    //    than one `dn/#`.
    let dn_grant = TopicFilter::DeviceDownSubtree { tenant, device: arm };
    assert!(dn_grant.covers(&cmd.topic()));
    assert!(dn_grant.covers(&stop.topic()));

    // 6. An inbound topic parses back to its leaf, so a firmware receive path
    //    dispatches on an exhaustive match. A fourth downlink leaf added to the
    //    contract breaks every such match at compile time.
    let raw = stop.topic().render();
    assert!(matches!(
        Topic::parse(&raw),
        Ok(Topic::DeviceDown { leaf: DevDown::Estop, .. })
    ));
    assert!(DevDown::Estop.is_preemptive());
    assert!(!DevDown::Cmd.is_preemptive());
    assert!(!DevDown::Cfg.is_preemptive());

    // 7. An identifier cannot forge a topic level, which is what makes the
    //    §9 subtree rules and the §5 replyTo MUST real checks.
    assert!(Seg::new("arm-7/../other").is_err());
    assert!(Seg::new("+").is_err());
    assert!(Seg::new("#").is_err());
}
```

Delete the `#[ignore = "PB-ARCH-01 gap — MQTT topics and device payloads exist only in docs"]` attribute. Leave `gap!` in place — eight Tier B tests still use it.

---

## 6. Risks and open questions

### 6.1 Clear these before anything else

**The workspace `Cargo.toml:11-17` comment block is wrong in four separate ways.** Verified today:

```
# NOTE: hcr / hcr_firmware are deliberately NOT members yet.
#   * hcr depends on hotaru_mqtt, which currently fails to build against
#     the hotaru checkout in this repo (hotaru_mqtt_broker asks hotaru_core for
#     feature `full`; hotaru_core 0.8.4 renamed it to `full_regex`).
#   * hcr_firmware is no_std and must never be built via `--workspace` — mixing
#     std and no_std members trips hotaru's own compile_error! guards.
# See docs/backend/04-SERVICE.md §10 and §1.
```

1. `crates/hcr` **is** a member — it is the third entry in `members` at `Cargo.toml:6`.
2. `hcr` does **not** depend on `hotaru_mqtt` at all. `crates/hcr/Cargo.toml:30-36` lists `hotaru`, `hotaru_http` and `hotaru_core` at `0.8.6`, all optional behind the `hotaru` feature. There is no `hotaru_mqtt` line.
3. There is no "hotaru checkout in this repo" — the deps are registry versions. And `04-SERVICE.md:217` records the `full`/`full_regex` mismatch as fixed on 2026-07-31.
4. `crates/hcr_firmware` does not exist: `ls crates/` → `hcr hcr_contract hcr_qbank hcr_sim hcr_ws`.
5. `docs/backend/04-SERVICE.md` does not exist; the file is `docs/04-SERVICE.md`. Same stale prefix at `hcr_contract/src/lib.rs:5`, `hcr_contract/src/wire.rs:5`, `hcr_ws/src/lib.rs:5`.

Delete the block in the Stage 1 PR. It actively misdirects anyone touching membership while adding transport code, which is Stages 2–4.

### 6.2 The Stage 2/3 dependency — **resolved upstream; this section replaces an incorrect earlier reading**

An earlier revision of this plan stated that the 0.8.2 → 0.8.6 forward port "was attempted and reverted",
citing `hotaru_mqtt` commits `14bf723`, `96f75ef`, `cea5f2a`, and concluded that Stage 2 had an unscheduled,
previously-abandoned upstream prerequisite. **That was wrong.** Those reverts are on the local working
branch (`audit/split-protocol-by-role`). The forward port itself is complete on
**`opub/theme-0.8.6-support`** (HEAD `592e001`), reached through the stepwise series `v085/a1-pin-deps`
… `v085/a8-bump-086` (PRs #109–#119).

Verified today by building that branch in a detached worktree:

| Check | Result |
| --- | --- |
| `Cargo.toml` pins | `hotaru_core = "=0.8.6"`, `hotaru_io_tokio = "=0.8.6"`, `hotaru_http = "=0.8.6"` |
| `cargo build` | Clean, against `hotaru_core v0.8.6` and `hotaru_io_tokio v0.8.6` |
| `cargo test` | **114 passed, 0 failed, 0 ignored** |
| Compatibility with `crates/hcr` | `hcr` pins `hotaru`/`hotaru_http`/`hotaru_core` at `0.8.6` (`crates/hcr/Cargo.toml:30-36`). `^0.8.6` and `=0.8.6` unify. **No conflict.** |

Each of the four breakages named in the earlier reading has a commit that addresses it: `96f75ef` follows
`TcpTransport` into `hotaru_io_tokio`, `3f7493b` moves the IO layer onto `HotaruRead`/`HotaruWrite`,
`01cf75f` brings `impl Protocol for MqttProtocol` up to the new trait, `cea7881` gives `impl Message for
Packet` its associated error type, `33bfffb` flushes the now-buffered writer.

**Two consequences that make Stages 2–4 substantially smaller than planned:**

1. **`ConnStream` is not something to implement from scratch — `hotaru_mqtt` is already generic over it.**
   `MqttChannel<W: ConnStream>` (`src/channel.rs:53`) and `Broker<W: ConnStream>` (`src/broker.rs:193`).
   Stage 2 is therefore: supply one `ConnStream` impl carrying MQTT over `hcr_ws`'s framing. Narrower than
   the 600–900 LOC estimate at `04-SERVICE.md:68-79`, which predates this branch.
2. **Stages 3 and 4 have passing upstream reference tests.** `multi_protocol_mqtt_works_alongside_http`
   (`tests/integration.rs:677`) registers MQTT alongside HTTP on one port with first-byte detection
   (`0x10` → MQTT) — that is Stage 3's exact shape. `external_broker_publish_reaches_mqtt_subscriber`
   (`:697`) has external code call `broker.publish` and MQTT subscribers receive the fanout — that is
   Stage 4's device bridge.

**Three upstream gaps are real and remain.** Re-verified on `theme-0.8.6-support`, not on `main`:

- **No retained-message store.** The `retain` flag round-trips on the wire (`src/packet.rs:68`,
  `src/broker.rs:538`) but delivery hard-codes `retain: false` (`src/broker.rs:516`) and nothing is stored
  or replayed to a late subscriber. `grep -rn 'retained\|retain_store\|retain_map' src/` → empty. **Four of
  the thirteen §4 topics specify retain** (`01-CONTRACT.md` §4), including `up/state` and `lwt`. Those
  semantics do not exist yet. This is the largest remaining upstream item and it lands in Stage 4.
- **No per-topic authorization.** Connection-level authentication *does* exist and is better than the
  earlier reading claimed: `pub trait Authenticator` with `async fn authenticate(&connect, remote_addr)`
  (`src/broker.rs:53-59`), and `Broker::new` installs `DenyAllAuthenticator`, not accept-all. But
  `grep -rn 'authorize\|Acl\|allow_subscribe\|permission' src/` → **0 hits**. §9's three ACL rules
  (`01-CONTRACT.md:362-377`) have no enforcement point. `TopicFilter::covers()` from Stage 1 is the right
  predicate for one; the hook to call it from does not exist upstream.
- **No tenant concept.** `grep -rin 'tenant' src/` → **0 hits**, on this branch as on `main`.
  `01-CONTRACT.md:163` claims `{tenant}` *"maps onto `hotaru_mqtt_broker`'s `TenantResolver` / `TenantId`"*.
  **That claim is false and should be corrected in `01-CONTRACT.md`, not worked around.**

**Scheduling consequence.** Stage 2 and Stage 3 are unblocked and should be re-estimated downward. Stage 4
carries the retained-store work, which is an upstream feature addition in a different repository — that is
where the real unscheduled work sits, not at the head of the chain. Stage 1 is unaffected either way.

### 6.3 Decisions the author must make before Stage 1 lands

1. **`DeviceCommand`'s normative shape.** TS (`hcr-v1.d.ts:685-692`) is a struct with `op: 'run'|'home'|'stop'|'resume'|'query'` plus optional `program`/`textCmd`; the Rust mirror (`hcr_v1.rs:837-854`) is a `#[serde(tag="op")]` enum with a sixth `Text` variant. `01-CONTRACT.md:3-4` calls both normative; `hcr_contract/src/lib.rs:4-6` says TS wins. This plan follows TS, which makes the mirror's `Text` variant a wire-incompatible bug — and leaves the gateway-translated ESP8266 with no documented `op` under which to carry `textCmd`. **This must be resolved, not silently chosen.**
2. **`dev.cfg.req` fields.** Prose says `{ axes, speedLimits, calibration }` (`01-CONTRACT.md:286`); schema says `{ axes: AxisConfig[] }` (`hcr-v1.d.ts:819`). Schema wins per `:4`, which means speed limits ride inside `AxisConfig.speedDegPerSec` and calibration has no transport at all. Confirm rather than drop.
3. **`AxisConfig.jointId`** — TS makes it required-but-nullable, the Rust mirror makes it a skipped `Option`. These produce different JSON.
4. **The LWT payload.** No `dev.lwt.*` kind exists in `HcrMessages`. `05-EMBEDDED.md:209` implies `Envelope<DeviceState>` with `status: 'offline'`, but that is inference. Related and unstated: two retained `DeviceState`s (`up/state` and `lwt`) can disagree, with no precedence rule. `device::will` encodes the inference and says so; approve or replace it.
5. **`{tenant}` and identifier grammar.** No charset, length bound or reserved-value rule exists for `{tenant}`, `{userId}`, `{deviceId}`, `{connId}`, `{matchId}`, `{service}` or `{method}`. `Seg` imposes MQTT's minimum (no `/`, `+`, `#`, NUL, empty) plus a 128-byte bound. If the project wants a stricter charset — and it probably should, for `{tenant}` at least — say so now; tightening later rejects previously-accepted ids.
6. **`AxisId` vs `ServoAxisId`.** The schema's device `AxisId` has six variants including a spare `T` (`hcr-v1.d.ts:644-645`); `hcr_contract/src/domain.rs:71-85` already ships `ServoAxisId` with five. This plan keeps both and documents the split; merging would reshape an already-serialized type.
7. **`serde_json` as an unconditional dependency.** `hcr_contract/Cargo.toml:11-13` has no `[dev-dependencies]` section, so firmware links the crate and relies entirely on LTO to drop a JSON parser it can never use (the MCU wire format is CBOR). Moving it behind `std` or into dev-deps is a one-line change but touches a shipped manifest and is not required for PB-ARCH-01. **Author's call.**

### 6.4 Risks that could invalidate the plan

- **`Program`'s f64 recursion.** `dev.cmd.req` carries `Option<Program>`, and `ProgramNode` is a recursive tree of `f64`-carrying nodes. Deriving `Deserialize` for that on xtensa LX106 — built with `-C target-feature=-div32,-mul32high`, `compiler-builtins-no-f16-f128`, `optimize_for_size`, in an 80 KiB DRAM budget, for firmware whose README says *"floating point is not pulled into the embedded image"* — pulls soft-float in, and the recursion is depth-unbounded, which is a stack hazard. The `topic.rs`/`device.rs` split **mitigates** this (routing-only firmware never names `DeviceCmdReq`) but does not solve it. A fixed-point mirror or a depth cap is real, unscheduled work before a Rust firmware links these types.
- **Two normative copies of the QoS/retain table.** `DevUp::qos()` etc. are a transcription of a Markdown table. Test 10 is what keeps them honest and is therefore load-bearing, not decorative — drift here is silent, and a telemetry topic quietly promoted to QoS 1 contradicts §4's own rationale on a 50 Hz stream.
- **The guarantee stops at the broker boundary.** `Envelope::new` is public and shipped, so `Envelope::new(id, "dev.cmd.req", ts, DeviceEstopReq { … })` published to a hand-built string still compiles. Sealing it would break every existing consumer. Defence is: no ergonomic reason to do it, plus a test asserting each `dev.*` kind string appears in exactly one `kind()` arm, plus the doc-conformance scrape. **The PR text must not describe this as a whole-program guarantee.**
- **Argument-order slips.** `estop(device_id, tenant, …)` compiles. Accepted deliberately: it renders a visibly wrong topic and fails the first integration test. The fix — transparent `Tenant(Seg)` / `DeviceId(Seg)` wrappers — is additive and wire-neutral if this turns out to be the first real bug.
- **`Topic<'a>` borrows.** A consumer wanting to store topics in a map or hold one across an await point must `render()` first. If Stage 4 or 6 wants owned topics pervasively, an owned `TopicBuf` gets added and the two types coexist.
- **`{kind}` may not stay open.** If it later turns out to be the envelope `kind` verbatim (`submission.result.evt`), the `UserEvent` constructor signature changes. Small break, but a break.
- **Contradictions inherited, not resolved:** the 1 MiB MQTT packet cap versus a ~2.5 MB Cutter Grid submission routed through the MQTT `submission.create.req`; §9 ACL rule 1 forbidding the `match/*/evt/*` subscriptions §4 requires; the bridge's telemetry republish silently promoted to QoS 1. All three predate this work and none is fixed by it.

### 6.5 Genuinely not known

- The comms-watchdog threshold (how many ms of silence before de-energizing) is specified nowhere in any doc read.
- Whether `hcr-fw` has a spare GPIO input for a hardware e-stop line. It has a `GpioController` trait with an IRAM ISR latch — the natural attachment point — but no pin is assigned, and the hardware was not investigated.
- Which of the two source-identical `hcr-fw` trees is actually flashed. Their build artifacts differ and only `/Users/jerrysu/hcr-fw` resolves its `../../bug_fix/hotaru` path dep.
- `05-EMBEDDED.md` §6 claims `hotaru_mqtt` is `#![cfg_attr(not(feature = "std"), no_std)]`. No local copy matches — all eight (v0.8.0–0.8.3) hard-depend on tokio and dashmap with no feature gate. Either the doc describes an unreleased variant or the claim needs re-verification before any firmware MQTT work is planned.
- Which limit table is physically correct for the actual arm. The two firmwares disagree on which axis is restricted (`hcr-fw`'s E axis min is 45.0°; `hotaru-esp8266`'s joint 2 is 55° and joint 5 is 0°).

---

## 7. What this does not do

**Criteria this chain touches:** `PB-ARCH-01` (Stage 1), `PB-SAFE-03` and `PB-SAFE-04` (Stage 5), `PB-HRI-01` (Stage 6).

**Criteria it does not touch at all:**

| Criterion | Why not |
| --- | --- |
| `PB-SAFE-01`, `PB-SAFE-02`, `PB-PATH-01`, `PB-PATH-02` | Already Tier A and passing (`paper_baseline.rs:95, 111, 139, 156`) |
| `PB-SAFE-06`, `PB-SAFE-07`, `PB-DET-01`, `PB-DET-02` | Already covered in `engine.rs` and `conformance.rs` |
| `PB-SAFE-05` (bounded contact force) | Needs a contact model carrying force or torque, plus ISO/TS 15066 Annex A and [P29]. Both are unread per `09-PAPER-BASELINE.md:131` |
| `PB-PERC-01` (moving workpiece) | Needs time-varying head pose in the replay loop — the largest engine change |
| `PB-MODEL-01` (root-anchored hair) | Needs strand identity; voxels are independent cells |
| `PB-MODEL-02` (scalp length field) | Needs length-over-surface targets and scoring |
| `PB-PATH-03` (coverage decomposition) | Needs a boustrophedon coverage planner |

**Not delivered by Stage 1 specifically:**

- **Preemption.** `PB-SAFE-03` stays red. §8 explicitly forbids relying on cross-topic ordering, so topic separation guarantees only that a stop is not behind a command *in the same per-topic FIFO*. Bounded-time preemption is a firmware receive-path property. The PR text must say this or it over-claims the criterion.
- **The hardware safety layer.** `05-EMBEDDED.md:306-319` names five non-software requirements — a hardware e-stop cutting servo power directly, mechanical limits, torque/current limiting, a de-energizing comms watchdog, and no tool sharper than the demonstration needs — and states plainly that *"the contract-level pieces… are necessary but **not sufficient**. They are interlocks against software faults, not against the mechanism."* Out of scope for the whole chain, and it belongs in a hardware safety review before any human-proximate use.
- **Any wire traffic.** Stage 1 compiles topics; nothing publishes one. No broker, no network, no hardware is touched.

**Explicitly out of scope for the chain as planned:** the external-broker fallback (mosquitto/EMQX), the in-process broker subscription hook (needs an upstream `hotaru_mqtt_broker` addition), TLS (`04-SERVICE.md:223`, deferred until certificates exist), `h2per`/`hotaru_grpc` (excluded from the hotaru workspace, treat as unavailable), the Autobahn conformance suite (named as standard at `04-SERVICE.md:106`, absent), a browser MQTT client (no `mqtt` dependency in the frontend), and adding topic types to `docs/schema/hcr-v1.d.ts` — the topic tree exists only as Markdown, and whether the TS schema must gain a counterpart before the Rust does is an open question, not an assumption.