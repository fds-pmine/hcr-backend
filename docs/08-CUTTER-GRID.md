# 08 — Cutter Grid: server-side verification

Cutter Grid is the second way to write a program. Instead of commanding joint angles, the player drives the
tool tip through a fixed world-axis lattice — *right 3, up 7, forward 3* — and the arm works out how to get
there. It is the easier mode to understand, which is why it exists, and the harder one to score, which is
what this document is about.

Types: [`crates/hcr_contract/src/cutter.rs`](../crates/hcr_contract/src/cutter.rs).
Verifier: [`crates/hcr_sim/src/cutter.rs`](../crates/hcr_sim/src/cutter.rs).
Frontend counterpart: `src/features/cutter-grid/` and SPEC v0.3 §15.

## 1. Why this is not just another program shape

The servo path works because the program fully determines the motion: the server reads
`SetJointAngle`, advances a controller on a fixed tick, and arrives at the same poses the browser did. The
program is the input and the motion is derived.

Cutter Grid inverts that. `right 3` does not say what the arm does — five joints have to be solved for, and
the solution is found by a damped-least-squares search over a multi-branch ladder of IK candidates that the
browser runs at compile time in a Web Worker. Two things follow:

1. **The motion is not derivable from the program** without redoing that search. Even then it would only
   match if both searches explored identically, which is a far stronger requirement than agreeing on
   arithmetic.
2. **The hair that comes off is swept along the realised joint path**, not along the ideal lattice line
   (`ladderPlanner.ts:583` sweeps between consecutive waypoints' end-effector positions). The two differ by
   up to the planner's own tolerance, which is enough to flip a voxel sitting on the boundary.

So the trajectory travels with the submission, and the server's job changes from *simulating a program* to
**auditing a claim**.

## 2. What is submitted

`SubmissionCreate` gains one optional field, `cutterGrid`, carrying both halves:

```ts
interface CutterGridSubmission {
  program: CutterGridProgram;      // what the player wrote
  plan: CutterTrajectoryPlan;      // what the browser planned from it
}
```

Both are needed and neither is redundant. The program is what the command cap applies to and what the server
re-expands; the plan is the motion that actually cuts. A submission with only the program could not be
scored the way the learner saw it; one with only the plan would have no way to charge commands.

Servo submissions are untouched — the field is absent and every existing byte on the wire is the same.

### Size

A trajectory is large. Measured on the shipped challenge's certified reference program (22 cells):

| | Value |
| --- | --- |
| Steps | 22 |
| Waypoints | 2 134 (194 entry + 1 940 program) |
| Uncompressed | ~900 kB |
| Worst case at the 500-command cap | ~2.5 MB, ~0.8 MB gzipped |

`MAX_REQUEST_BODY_BYTES` is 8 MiB, and **the two reverse-proxy configs in `deploy/` must match it**. The
smallest of the three is the real limit, and if that one is the proxy the client gets a bare 413 with no
`HcrError` body and nothing in the service log to explain it.

## 3. What the server re-derives

Everything below comes from `jointAngles` alone. The client's `endEffector`, `expectedCutVoxels`,
`expectedResultVoxels`, `executedCommandCount` and `trajectorySignature` are compared against the server's
own answer and never substituted for it.

| Check | The lie it stops |
| --- | --- |
| Challenge signature (§5) | Planning against a kinder arm or an easier hairstyle |
| Steps match the re-expanded IR | Submitting a short program with a long trajectory |
| Coordinate chain | Teleporting, skipping or repeating cells |
| Joint limits | Poses the physical arm cannot reach |
| Head collision, every waypoint | Cutting through the customer |
| Forward kinematics vs declared tip | Declaring a safe path while claiming a cutting pose |
| Pose continuity across steps | Jumping between steps, which sweeps everything in between |
| Per-step axis displacement | Calling a sweep across the whole head "right 1" |
| Path deviation | Bulging a cell-to-cell move out through the hair and back |
| Entry cuts nothing | A free haircut before the clock starts |
| Waypoint ceiling | A submission that is syntactically fine and computationally ruinous |

Then the score: sweep the tool sphere between consecutive server-computed tip positions, remove what it
touches, and hand the result to the existing scorer. Nothing about scoring is Cutter Grid-specific — the
same `calculate_score`, the same `ScoringConfig` from the challenge, the same `ScoreResult` shape.

A trajectory that survives all of this is one the arm could really have flown, whatever the client intended.
That is what makes the resulting score authoritative **without the server running the IK search itself**,
which is the whole design.

## 4. The trust boundary, stated plainly

`trajectorySignature` is `fnv1a64` over the plan minus that field. A client that fabricates a plan can
trivially compute a matching signature. **It detects corruption, never forgery**, and nothing in the
verifier treats it as authentication. The same is true of `challengeSignature` as a value — what makes that
one useful is that the server recomputes it from its own copy of the challenge (§5) rather than believing it.

The audits in §3 are the actual boundary. They were chosen so that passing them is equivalent to the
trajectory being physically legal, which means a successful forgery is indistinguishable from an honest
plan — because it *is* one.

Two things are deliberately outside the boundary:

- **Optimality.** The server does not check that the trajectory is the one the planner would have produced,
  only that it is a legal one. A client that found a better path within the rules has not cheated; it has
  played well.
- **The IK search itself.** Reproducing it server-side is the only way to make the trajectory derivable
  rather than merely checkable, and it is a much larger piece of work — a float-level port of the ladder
  planner. It is the natural next part, not this one.

## 5. Challenge signature

`cutter_grid_challenge_signature_v2` ([`signature.rs`](../crates/hcr_contract/src/signature.rs)) is a port
of `features/cutter-grid/signature.ts`. It hashes joint travel, arm and collision dimensions, lattice
placement, the head ellipsoid, tool radius, both hairstyles, and the planner's own search constants.

The port writes its JSON by hand. The hash is taken over `JSON.stringify` of a JavaScript object literal, so
the bytes depend on key order and on JavaScript's number formatting — `serde_json` agrees on neither, and
would print `45.0` where JavaScript prints `45`. The field order in that file is normative.

This is pinned by `shipped_challenge_matches_the_bundled_profile`, which asserts the shipped challenge
hashes to `7d5a4afd61db49ea` — the value carried by the frontend's own certified profile. Every rule the
port encodes is only checkable in aggregate, because the digest is all the frontend exposes. One wrong byte
and no Cutter Grid submission would ever be accepted, with nothing but `SIGNATURE_MISMATCH` to say why.

**Editing a challenge invalidates every plan made before the edit.** That is the intended behaviour and the
main thing the signature buys: retuning joint travel is an ordinary content change, and without it every
trajectory planned against the old arm would keep scoring against an arm that no longer exists.

## 6. Rejection, not partial credit

The servo engine stops at the last safe pose and still produces a score, because a collision there is
something the learner's program *did*. Cutter Grid refuses the whole submission instead, because the
frontend will not run such a program at all (SPEC v0.3 §15.3 — planning failures are located to the first
offending block and the entire program is rejected before execution). A server that scored a partial Cutter
Grid run would be scoring something no client would ever produce.

Failures surface as `TRAJECTORY_REJECTED` (HTTP 422), with `details.rejection` naming which audit failed and
`field` carrying the block to highlight when one can be attributed:

```json
{
  "error": {
    "code": "TRAJECTORY_REJECTED",
    "message": "The planned path would touch the head.",
    "retryable": false,
    "field": "reference-3",
    "details": { "rejection": "HEAD_COLLISION" }
  }
}
```

`TRAJECTORY_REJECTED` is distinct from `PROGRAM_INVALID` on purpose. The latter means the program is
malformed. Here the program is fine and the trajectory planned from it did not survive verification — a
different fault, with a different fix (replan, usually by reloading), and unlike a bad program, not
something the learner caused.

The thirteen rejection kinds live in `details` rather than each getting a wire code. Thirteen codes would be
thirteen things for every client to learn; a client that wants to distinguish them reads one key, and one
that does not still gets a usable code.

## 7. Divergence

`CutterDivergence` compares the client's `executedCommandCount` and `expectedResultVoxels` against the
server's. It is **observational**: a mismatch sets `replay.divergedFromClient` and nothing else. A learner
is never told their browser disagreed with the server, because it is not something they can act on — this is
a conformance alarm for operators, exactly as `ClientPreview` works on the servo path
([`02-DETERMINISM.md`](02-DETERMINISM.md) §4).

Note what is *not* enforced: a wrong `executedCommandCount` is reported, not refused. The field is advisory,
and rejecting on it would turn a telemetry signal into an outage the first time the two expanders disagreed
about something harmless.

## 8. Usage log

A `submission` row gains an optional `mode`, absent (and meaning `servo`) on every row written before this
existed. The two modes are not comparable — a Cutter Grid command is one cell of tool travel, a servo
command is one joint move — so pooling them would fit item difficulty against a mixture of two different
tasks and call the result one number. SPEC v0.3 §15.1 already says the scores are not to be compared for
fairness; this is what lets an analysis honour that.

Compatibility rules, and the tests that hold them, are documented in
[`crates/hcr_service/src/usage.rs`](../crates/hcr_service/src/usage.rs).

## 9. What this does not change

Deliberately untouched, per SPEC v0.3 §15.4:

- Servo `Program`, `RobotCommand` and `ProgramNode` — frozen, and not extended to carry lattice moves.
- `ScoreResult`, `ProgramMetrics`, `SubmissionResult` — one shape for both modes, so everything downstream
  is indifferent to which engine ran.
- The frontend. Submit stays disabled in Cutter Grid mode and scoring stays local until the server has been
  observed agreeing with the browser on real programs.

## 10. Sessions and rounds

Both accept Cutter Grid, and both are **single-mode**.

| | Rule |
| --- | --- |
| Round | `MatchConfig.programmingMode` is declared at creation; a submission scored in another mode is refused with `WrongProgrammingMode`. Creation fails if the challenge does not support the mode, and unpinned selection only considers challenges that do. |
| Session | `SessionStart.programmingMode` is fixed for the session's lifetime. The bank is filtered to items declaring support, and `session.respond` refuses a submission scored in another mode. |
| `ChallengeMeta` | `programmingModes` lists what an item can be attempted in. Defaults to servo alone — Cutter Grid needs a certified planner profile, which the item generator does not produce. |

The ability estimate is **per programming mode**, mirroring the existing rule that match play never touches
θ_solo: same library, same scale, different task. That is not a simplification pending better plumbing — it
is what the measurement model permits today. Putting both modes on one difficulty scale needs a mode offset
γ, γ needs linking items served in both, and exactly one shipped challenge has a certified profile against
the ≥ 15 the design asks for. The rules above are what stops a Cutter Grid attempt silently moving a servo
ability. Full reasoning and what would unblock it: [`07-CALIBRATION.md`](07-CALIBRATION.md) §11.

Every usage row carries its mode, so the responses needed to estimate γ are being collected now.
