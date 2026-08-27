# hcr_contract

The `hcr.v1` wire contract for the HCR Simulator: one set of request, response
and domain types shared by the backend service, the replay engine and device
firmware.

The normative schema is the TypeScript file `docs/backend/schema/hcr-v1.d.ts` in
the project repository. When this crate and that file disagree, the TypeScript
definition is the contract and the Rust side is the bug.

## Usage

```toml
[dependencies]
hcr_contract = "0.3"
```

Firmware and other `no_std` targets drop the default feature and keep `alloc`:

```toml
[dependencies]
hcr_contract = { version = "0.3", default-features = false }
```

| Feature | Default | Effect |
| --- | --- | --- |
| `std` | yes | Enables `std` in `serde` and `serde_json`. Without it the crate is `no_std + alloc`. |

The crate sets `#![forbid(unsafe_code)]`.

## Naming is not a style choice

The wire is `camelCase` everywhere, so every type carries
`#[serde(rename_all = "camelCase")]` and the TypeScript definitions can be read
literally against these structs. A field renamed on one side without the other
is a protocol break, not a formatting preference.

## Challenge signatures

`cutter_grid_challenge_signature_v2` reproduces the frontend's FNV-1a digest over
the exact document the browser hashes, down to key order and JavaScript's
integer formatting. A plan built under one challenge cannot be replayed as
though it were built under another, and one wrong byte would reject every
Cutter Grid submission with nothing but `SIGNATURE_MISMATCH` to explain it.
`signature_document` exposes the string being hashed so a mismatch can be
diffed rather than guessed at.

## Related crates

- [`hcr_sim`](https://crates.io/crates/hcr_sim): deterministic simulation core
- [`hcr`](https://crates.io/crates/hcr): backend service built on both

## Requirements

Rust 1.85, edition 2024.

## License

MIT
