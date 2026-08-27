# hcr_sim

Deterministic simulation core for the HCR Simulator: kinematics, head-safety
checks, voxel sweep and scoring. A Rust port of the frontend engine, shared by
server-side authoritative replay and by device firmware as `no_std + alloc`.

The browser keeps its own TypeScript copy, and that copy remains the definition
of correct. Conformance vectors are generated from it, and this crate is
verified against them.

## What determinism means here

A replay advances by a fixed `hcr_contract::SIM_TICK_MS` and never reads a wall
clock, so a run is a pure function of `(challenge, program)`. Voxel sets are
`BTreeSet`, giving specified iteration order instead of hash-seed dependence.
`sin` and `cos` come from `libm` in every build, std or not, so a server and a
microcontroller agree bit for bit.

Agreement with JavaScript is a weaker claim, and the crate is explicit about it:
IEEE-754 does not require correctly-rounded transcendentals, so the two engines
match to within a few ULP. That is why divergence between them is measured by
Jaccard distance over the resulting voxel sets rather than by hash equality.

## Usage

```toml
[dependencies]
hcr_sim = "0.3"
```

Firmware builds drop `std` and keep replay and the safety checks:

```toml
[dependencies]
hcr_sim = { version = "0.3", default-features = false }
```

| Feature | Default | Effect |
| --- | --- | --- |
| `std` | yes | Enables `std` in `hcr_contract`, `serde` and `sha2`. |
| `planner` | no | Links the compact Cutter Grid V4 planner (IK search and trajectory selection). Server-only, and implies `std`. |

The crate sets `#![forbid(unsafe_code)]`.

## Modules

`kinematics` and `collision` model the arm and its head-safety constraint.
`cutter` and `scoring` decide what the tool removes and what that run is worth.
`engine`, `executor`, `controller` and `program` run a compiled program forward.
`replay` is the entry point the server uses.

With `planner`, `cutter_grid_v4` adds endpoint IK enumeration, PTP primitive
certification and the compact path search that produces a frozen trajectory
plan.

## Related crates

- [`hcr_contract`](https://crates.io/crates/hcr_contract): the wire types this crate speaks
- [`hcr`](https://crates.io/crates/hcr): backend service that replays through it

## Requirements

Rust 1.85, edition 2024.

## License

MIT
