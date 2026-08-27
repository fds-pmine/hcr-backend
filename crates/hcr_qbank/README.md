# hcr_qbank

Adaptive question bank for HCR Simulator challenges, built on the
[arona](https://crates.io/crates/arona) CAT engine.

## What arona provides, and what it leaves open

arona supplies the measurement core: IRT models, ability estimation, item
selection strategies, termination rules and session orchestration. Its bundled
`StaticQBank` is deliberately minimal. The pool is fixed at construction with no
add or remove, there is no exposure control, `SelectionHints::used_types` is
ignored, and it reaches for `thread_rng()`, which makes a session impossible to
reproduce.

This crate implements arona's own `QuestionBank` trait to close those gaps. The
trait is object-safe with three required methods, so a replacement bank is the
intended extension point rather than a workaround.

| Need | Where it lives |
| --- | --- |
| θ estimation, Fisher information, termination | arona |
| Mutable item pool, exposure control, content blueprint | `bank` |
| Continuous score to arona's dichotomised `Score` | `mastery` |
| Async replay against arona's synchronous `score()` | `content` |
| Item identity across a changing pool | `bank::ServedItem` |

## Two constraints that shaped the design

`Score::new` panics outside `[0,1]`, so every value handed to it is clamped
before the call rather than assumed to be in range.

`QuestionContent::score` is synchronous and infallible, but scoring an HCR
response means replaying a program, which is neither. The service therefore
replays first and stores the authoritative score, and the synchronous call reads
that stored result. The ordering is fixed, not incidental.

## Usage

```toml
[dependencies]
hcr_qbank = "0.3"
```

## Related crates

- [`hcr`](https://crates.io/crates/hcr): the service that drives sessions
- [arona](https://crates.io/crates/arona): the CAT engine underneath

## Requirements

Rust 1.85, edition 2024.

## License

MIT
