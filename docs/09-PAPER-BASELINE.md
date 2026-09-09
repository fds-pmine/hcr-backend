# 09 — Paper Baseline

What the published robotic-haircutting literature requires, what this system already does, and what it
does not. The point is to measure the system against an external standard rather than against its own
behaviour, so that a claim made in a paper about this system is one the test suite can discharge.

Every criterion has an id (`PB-<area>-<n>`) and a test. The tests live in
[`crates/hcr_sim/tests/paper_baseline.rs`](../crates/hcr_sim/tests/paper_baseline.rs) and run in two tiers:

| Tier | Meaning | How it runs |
| --- | --- | --- |
| **A** | Criterion met. This is a claim the system is entitled to make. | Ordinary suite; must stay green |
| **B** | Criterion set by the literature and not met. | `#[ignore]`d; each panics with the missing capability |

```sh
cargo test -p hcr_sim --features planner --test paper_baseline              # Tier A: 4 passed
cargo test -p hcr_sim --features planner --test paper_baseline -- --ignored # Tier B: the gap checklist
```

A Tier B test graduates by being rewritten against the API that closed it, never by being deleted.

## 1. Sources, and what each is good for

The robotic-haircutting corpus is **entirely** secondary. All six works below are surveys, reviews or
perspective pieces — [S1] included, once its preprint was read — and a seventh plenary entry is a two-page
abstract. **A survey cannot establish a criterion.** It reports what a field has done; it does not create an obligation, and citing one in place of
the work it summarizes is a citation error. The criteria in §2 are therefore traced to primary sources
(§7); the secondary corpus is retained for the one thing it *is* authoritative for.

### What surveys are authoritative for

**Absence claims.** "No published system does X" is exactly the assertion a systematic review is
constructed to support, and it is where this project's differentiation argument lives (§3). [S2]'s search
methodology is explicit and reusable: 820 hits for hair simulation/modeling/editing across IEEE Xplore, ACM
DL, Scopus and Google Scholar, against 30 papers on brushing, combing, washing and scalp massage after
de-duplication and screening — with robotic *haircutting* itself described as "largely unexplored". Cite
that for the size of the gap. Do not cite it for how to build anything.

### Secondary corpus

| Ref | Work | Type | Use it for |
| --- | --- | --- | --- |
| **[S1]** | Huang, Li & Röning, *Safety in Robotic Haircutting*, **Journal of Field Robotics** (Wiley, ISSN 1556-4959) 2026 — [10.1002/rob.70305](https://doi.org/10.1002/rob.70305) | **Survey** — self-labelled | The three-layer safety framework; the ISO hooks; **its §7 gap statement** (§6 below). Preprint held locally: [10.22541/au.175917093.39470494/v1](https://doi.org/10.22541/au.175917093.39470494/v1) |
| **[S2]** | Li, *Haircutting Robots: From Theory to Practice*, Automation 2025, 6, 47 — [10.3390/automation6030047](https://doi.org/10.3390/automation6030047) | Perspective / review | Absence claims; the head-as-workpiece framing; **its bibliography**, which is this document's main yield |
| **[S3]** | Khan & Li, *Robotic Haircutting Systems: A Survey*, IEEE JSAS 3 (2026) 104–112 — [10.1109/JSAS.2026.3654480](https://doi.org/10.1109/JSAS.2026.3654480) | Survey | Little. See the warning below |
| **[S4]** | Khan & Li, *CNC-Inspired Robotic Hair Cutting: A Comprehensive Survey*, JAIA 2026 | Survey | Framing only |
| **[S5]** | Li, *Haircutting Robots: A Mobile Robotics Perspective*, Int. J. Systems Science 2026 | Perspective | Framing only |
| **[S6]** | Li, *Haircutting Robots*, SpringerBriefs 2025 — [10.1007/978-3-031-84026-5](https://doi.org/10.1007/978-3-031-84026-5) | Monograph | System blueprint; a book chapter, not a peer-reviewed result |

> **Correction — [S1] is a survey too, and that finishes the argument.** An earlier revision recorded [S1]
> as "reportedly primary, proof-of-concept demonstrations… the only work in the corpus with experiments."
> The preprint is now held locally and that is false on three independent checks: the manuscript header
> reads **SURVEY ARTICLE**; the abstract's first move is "This paper **reviews** the safety landscape";
> and there is no experiment, apparatus or measurement anywhere in its 920 lines.
>
> **So all seven works by this group are secondary. The corpus contains zero primary results.** Every
> quantitative claim in it is relayed from somewhere else, which is why §7 exists and why nothing may be
> cited to [S1]–[S6] that could instead be cited to the work they are relaying.
>
> Two further defects in [S1], recorded because they bear on how much weight the group's output can carry:
>
> - **The running head on every odd page reads "Haircutting Robots: A Mobile Robotics Perspective"** —
>   that is **[S5]'s** title, not this paper's. The manuscript was built from [S5]'s file and the header
>   was never changed. The same template is being re-run across outputs.
> - **The author list does not match.** The preprint (29 Sep 2025) is Huang, Shuai Li, Röning — three
>   authors, **no Vilkki**, whom an earlier revision of this document listed on the strength of a search
>   summary. Confirm the byline against the Wiley version of record before citing.
>
> Refs 45–48 of [S1] are news articles — *The Economic Times*, **BBC on the Boeing 737 Max**, *The Verge*,
> *The New York Times* — carrying the argument in a robot-safety review.

> **"JFR" here means the *Journal of Field Robotics*** (Wiley, ISSN 1556-4959, DOI prefix `10.1002/rob`).
> It is not the *Journées Francophones de Radiologie*, which a plain web search for the acronym returns
> first. Every venue abbreviation in this document is expanded on first use for that reason.

A seventh item, Li, *Haircutting Robots: from Idea to Practice*, CINTI 2025 plenary
([10.1109/CINTI67731.2025.11311807](https://doi.org/10.1109/CINTI67731.2025.11311807)), is a two-page
abstract and speaker CV with no technical content. Its only substantive claim is that no commercially
available haircutting robot exists. Do not cite it for anything else.

### Warning: [S3]'s bibliography does not survive checking

Verified defects in the reference list of [S3], found while mining it for primaries:

- Entries **[19]** and **[22]** cite the *same* YouTube URL (`watch?v=7zBrbdU_y0s`) under two different
  titles and two different authors. One of those authors, "S. M. Here", is the channel name *Stuff Made
  Here* parsed as a person.
- Entry **[8]** is malformed and truncated: `S. Li, "Haircutting robots.` — no venue, no year.
- Entry **[37]** dates *AI & Society* vol. 38 to 2024; that volume is 2023.
- Entries **[35]**, **[36]**, **[37]**, **[38]** and **[46]** share a pattern — two generic surnames, round
  page ranges (`234–241`, `456–468`, `789–801`, `1123–1138`), and titles that restate their own venue's
  scope. None of them could be matched to a real record while assembling §7.

Treat every [S3] citation as unverified until resolved against the publisher. Where [S3] and [S2] cite the
same primary work, §7 uses [S2]'s record, which checks out. This is also worth knowing for venue choice:
§6 recommends IEEE JSAS, and the state of this survey's bibliography is evidence about that journal's
review process, not just about these authors.

## 2. Criteria

### Tier A — met

| Id | Criterion | Primary source | Test |
| --- | --- | --- | --- |
| `PB-SAFE-01` | A motion is not emitted unless positive head clearance is **proven** across the primitive, not sampled at its endpoints | [P17], [P20] — [P20] is literally "certified safety" for the avoidance constraint | `pb_safe_01_certified_plans_prove_positive_head_clearance` |
| `PB-SAFE-02` | Velocity, acceleration and jerk limits are **satisfied**, not merely recorded | No primary in this corpus; absence confirmed by [S3]/[S4] | `pb_safe_02_dynamic_limits_are_satisfied_not_merely_recorded` |
| `PB-PATH-01` | Certified poses keep a margin against joint limits; the plan never relies on a mechanical hard stop | [P18], [P19] — redundancy resolution under limits | `pb_path_01_certified_plans_keep_a_joint_limit_margin` |
| `PB-PATH-02` | The tool removes only intended material; approach motion removes none | [P25], [P26] — machining framing | `pb_path_02_the_tool_removes_only_intended_material` |

`PB-SAFE-02` is the one Tier A criterion with **no primary source to satisfy**, in this corpus or its
references. That is not a weakness of the criterion; it is the finding. Jerk-bounded, certificate-carrying
motion near a person is standard practice in industrial robotics and absent from this literature, which is
why §6 builds the paper around it.

Four further criteria are discharged by tests that already existed. They are listed here so the mapping is
complete, and are deliberately **not** duplicated in `paper_baseline.rs`:

| Id | Criterion | Existing test |
| --- | --- | --- |
| `PB-SAFE-06` | Contact halts at the last safe pose and is never silently corrected | `engine.rs::driving_the_arm_into_the_head_stops_at_the_last_safe_pose` |
| `PB-SAFE-07` | The safety boundary is a geometric property, independent of control-loop rate | `engine.rs::collision_stop_angle_is_independent_of_tick_size` |
| `PB-DET-01` | A run is a pure function of (challenge, program) | `engine.rs::replay_is_deterministic_across_runs` |
| `PB-DET-02` | Independent implementations of the engine agree | `conformance.rs::rust_engine_matches_the_typescript_engine` |

> **The head contact-force number is in dispute; resolve it before it reaches `PB-SAFE-05`.**
> [S1] §4 states that "according to ISO/TS 15066, the allowable transient contact force for the human head
> is approximately **65 N**, with pressure thresholds around **90 N/cm²**", citing [P29]. But ISO/TS 15066
> Annex A gives **130 N** for the skull-and-forehead region — exactly twice [S1]'s figure. One of the two
> is wrong, and a safety threshold that is out by 2× in the permissive direction is the worst possible
> defect for this project to inherit. **Take the value from ISO/TS 15066 Annex A and [P29] directly.**
> Neither has been read for this document; both must be before `PB-SAFE-05` is implemented.

`PB-DET-01`/`PB-DET-02` have no counterpart in [S1]–[S6]. They are this system's own addition to the
baseline, and §5 argues they are the most defensible thing it has.

### Tier B — not met

| Id | Criterion | Primary source | Missing capability |
| --- | --- | --- | --- |
| `PB-HRI-01` | Execution is compared against a **consented** trajectory; deviation escalates warn → pause → hand over | [P15], [P16] — the shared-control work [S2] leans on | A monitor over planned vs executed trajectories, publishing with hysteresis |
| `PB-SAFE-03` | E-stop preempts queued motion within bounded time | [S1] (unobtained); `01-CONTRACT.md` §6.5 | An interrupt input on the executor, ahead of the command queue |
| `PB-SAFE-04` | Loss of communication de-energizes or holds position | [S1] (unobtained); `05-EMBEDDED.md` §7 | A device-side watchdog independent of the broker |
| `PB-SAFE-05` | Scalp contact force is bounded | **[P29]** (biomechanical limits, Fraunhofer IFF) and **ISO/TS 15066** Annex A for the threshold; **[P1]** for measured forces during *hair brushing* specifically; **[P30]** for the 43–45 °C thermal limit. **Do not take the number from [S1]** — see below | Any force or torque in the contact model |
| `PB-PERC-01` | The head may move during execution | [P12], [P13] head pose; [P14] disturbance observer | Time-varying head pose in the replay loop, and compensation |
| `PB-MODEL-01` | Hair is anchored at the root | **[P2]** — root-centric strand adjustment, with path planning | Strand identity; voxels are independent cells |
| `PB-MODEL-02` | The cut target is a scalar length field over the scalp | [P7] mass-spring; [P8]–[P11] strand models | Length-over-surface targets and scoring |
| `PB-PATH-03` | Coverage is decomposed into passes | [P25], [P26] — **both reviews**; see the gap note below | A coverage planner alongside the authored-traversal one |
| `PB-ARCH-01` | The device transport grammar is typed, not prose | `01-CONTRACT.md` §5–6.5 | Topic constructors and device payload types |

**Coverage-planning gap.** For `PB-PATH-03` the corpus offers only [P25] and [P26], and both are themselves
reviews — no survey here cites a primary coverage-path-planning result. If that criterion is ever pursued,
the literature to read (Choset's coverage decomposition line, and boustrophedon cellular decomposition) has
to be reached from outside this corpus entirely. Do not cite [S4] as if it supplied it.

## 3. Alignment

Three things this system does that the surveyed literature does not.

**A certificate-carrying planner.** Every V4 plan ships proven minimum head clearance, minimum joint-limit
margin, and velocity/acceleration/jerk ratios against hard limits
(`hcr_contract/src/cutter.rs:583-615`). [S3] and [S4] survey the field's control and safety practice
without describing any concrete safety architecture — no e-stop, no watchdog, no jerk bound. `PB-SAFE-01`
and `PB-SAFE-02` are the assertions that turn those recorded numbers into guarantees, which is the gap
between reproducing a certificate and honouring one.

**Reproducibility as an engineering property.** One Program IR, three executors, and cross-language
conformance vectors that make the browser, the server and the firmware agree bit-for-bit
(`02-DETERMINISM.md`). `libm` in every build so std and `no_std` share one trig implementation; `BTreeSet`
so iteration order is specified. Nothing in [S1]–[S6] treats reproducibility as a property to be enforced.

**A control layer that already reads like [S1]'s middle tier.** Contact stops at the last safe pose and
enters an error state rather than being corrected; the boundary converges to the same geometry regardless
of tick size. `05-EMBEDDED.md` §7 then enumerates [S1]'s hardware layer — direct-cut e-stop, mechanical
limits, torque limiting, comms watchdog — and states plainly that the simulator's geometry is not a safety
system. The framework is understood; two of its three layers are not implemented.

**One correction to a claim worth not repeating.** The voxel model is *material removal*, not hair physics.
[S3]'s observation that the field models hair appearance and not hair mechanics is real, but a voxel sweep
does not fill that gap — it is the CNC side of the analogy, not the mechanics side. `PB-MODEL-01` and
`PB-MODEL-02` are where the actual hair-modeling gap sits.

## 4. Gaps, in fill order

Ranked by publishable value per unit of work, not by difficulty.

**1. `PB-ARCH-01` — type the transport contract.** Small, unblocks everything else, and turns a documented
rule into a compiler-checked one. See §5.

**2. `PB-SAFE-03` + `PB-SAFE-04` — e-stop and watchdog.** These are [S1]'s framework applied to hardware
this project actually owns, and most of the work already exists in the wrong repository. `hotaru-esp8266`
owns arm/e-stop/motion state in Rust (`firmware/HotaruArm8266/HotaruBridge.h:55`), exposes both an
emergency stop and an actuator disable, and rate-limits motion to 1° per 20 ms tick
(`rust_core/src/state.rs:17-18`). `hcr-fw` — the firmware that has actually been flashed — has a watchdog
feed and a panic handler and **no e-stop, arm or disarm at all**. So the work is a port plus a `dn/estop`
subscription. Highest safety value, and the prerequisite for any claim involving a real arm.

**3. `PB-HRI-01` — deviation monitor.** [S2]'s supervised-autonomy loop: compare executed against consented
trajectory, escalate with hysteresis. A few hundred lines over the typed topics from step 1, testable
entirely in simulation, and it gives `dn/estop` a reason to fire that is not a human hitting a button.

**4. `PB-PERC-01` — moving head.** The largest change to the engine: `VoxelConfig` fixes head pose for the
whole replay, so this reaches determinism, scoring and conformance vectors at once. Do it only with a plan
for what it does to the existing vectors.

**5. `PB-MODEL-01`, `PB-MODEL-02`, `PB-PATH-03`, `PB-SAFE-05`.** Research-scale. Each is a separate piece of
work with its own literature; none is on the path to a paper about the current system.

Note what is *not* here: closing every gap is not the goal. `PB-PATH-03` is arguably not a gap at all in a
teaching product, where choosing the traversal is the exercise rather than something to be solved for the
learner. Deciding a criterion does not apply is a legitimate outcome — record the decision, keep the test
ignored, and say why in the paper.

## 5. Transport: what "MQTT is not wired up" means precisely

The contract is complete, the hard half of the implementation is done and tested, and the two are not
connected to each other.

| Piece | State | Evidence |
| --- | --- | --- |
| Topic tree, QoS/retain policy, device payloads, e-stop-on-its-own-topic rule | **Specified** | `01-CONTRACT.md` §5, §6.5 |
| Adapter design, handshake path, size bounds, 600–900 LOC estimate | **Specified** | `04-SERVICE.md` §3 |
| RFC 6455 framing for the MQTT binding — handshake, masking, fragmentation, ping/close, size limits | **Implemented, 33 tests** | `crates/hcr_ws/` |
| MQTT 3.1.1 broker and client | **Available**, separate crate | `hotaru_mqtt` on `opub/theme-0.8.6-support`, 114 tests green. Connect-time auth exists (`Authenticator`, deny-all default). Missing: **retained-message store** (delivery hard-codes `retain: false`, `broker.rs:516`), **per-topic ACL**, **tenant concept** |
| `ConnStream` / `HotaruRead` / `HotaruWrite` adapter binding the two | **Absent here, but smaller than specified** | No `ConnStream` reference under `crates/`. Upstream `hotaru_mqtt` is already generic over it (`MqttChannel<W: ConnStream>`), so the job is one impl over `hcr_ws` framing, not the 600–900 LOC of `04-SERVICE.md` §3 |
| Broker instantiation and protocol registration | **Absent** | `crates/hcr/src/deploy.rs:209-212` registers `single_protocol(HTTP::server(...))` only |
| Device bridge (loopback client subscribing `up/#`) | **Absent** | Design in `04-SERVICE.md` §8 |
| `dn/estop` publisher or handler | **Absent** | No `hcr/v1/` string in any `.rs` under `crates/` |
| Browser MQTT client | **Absent** | No `mqtt` dependency in the frontend |

So the remaining server-side delta is the adapter named in `hcr_ws/src/lib.rs:17-20` — deliberately left
outside that crate — plus registration and the bridge. The framing layer, which is the spec-heavy part, is
behind us.

**Two stale comments to clear first, because both point the wrong way:**

- Workspace `Cargo.toml:11-14` says `hcr` is deliberately excluded from `members` because `hotaru_mqtt`
  fails to build against the in-repo hotaru checkout. Both halves are now false: `crates/hcr` is the third
  entry in `members`, and `04-SERVICE.md` §10 records the feature mismatch as fixed on 2026-07-31.
  `hotaru_mqtt`'s **checked-out** branch pins `hotaru_core "=0.8.2"`, while `crates/hcr` pins `0.8.6`
  (`crates/hcr/Cargo.toml:30-36`) — but that is a working-branch state, not the project's state. The
  0.8.6 forward port is complete on **`opub/theme-0.8.6-support`**, which pins `hotaru_core`,
  `hotaru_io_tokio` and `hotaru_http` at `=0.8.6` and, built in a worktree on 2026-09-09,
  **compiles clean and passes 114 tests**. `^0.8.6` and `=0.8.6` unify, so there is no conflict with
  `crates/hcr`. Two earlier revisions of this document got this wrong in opposite directions; see
  `10-PB-ARCH-01-PLAN.md` §6.2 for the verified account.
- The same note excludes `hcr_firmware`, which is not in the tree.

**Suggested order.** Type the grammar in `hcr_contract` first (`PB-ARCH-01`): topic constructors and the
`dev.*` payload types from §6.5, with the e-stop/command separation expressed so the compiler enforces it.
That is testable with no broker, no network and no hardware, it converts one Tier B test to Tier A on its
own, and every later step — adapter, bridge, deviation monitor, firmware — consumes it.

## 6. Where this publishes

The contribution that survives contact with §2 is not the robot. It is **a reproducible, certificate-carrying
control and simulation stack for robotic haircutting**, in a field whose entire published output — all seven
works in §1 — is secondary, and which describes no concrete safety architecture and no reproducibility
practice at all.

**The field has now stated this gap in its own words.** [S1] §7, "Research Gap and Future Work", opens with
*"Lack of quantified safety thresholds"* and continues:

> "A crucial next step is the formulation of task-oriented evaluation protocols, including **standardized
> test cases, acceptance criteria, and reporting formats**." — and, separately, that ISO 10218 and
> ISO/TS 15066 "do not cover the unique requirements of haircutting robots."

Standardized test cases with acceptance criteria and a reporting format is a literal description of
`paper_baseline.rs`: Tier A is the acceptance criteria, Tier B is the reporting format for what is not yet
met, and the conformance vectors are the test cases. Cite [S1] §7 as the statement of need and the suite as
the response. That is a far stronger framing than differentiation-by-absence, because the incumbent authors
are the ones asking for it.

The claim to make: *haircutting robot control can be specified as a wire contract with machine-checkable
motion certificates, and independent implementations can be held to it by conformance vectors.* Evidence:
the Tier A table, the conformance suite, and the certificate schema. Limitations, stated up front: the
model is geometric, the head is static, validation is in simulation.

| Target | Fit | Risk |
| --- | --- | --- |
| **IEEE JSAS** — *primary* | Published [S3], so the editors are demonstrably receptive to this topic; the "standards" remit fits a contract-and-conformance paper better than any pure robotics venue; open access | New journal, limited track record |
| **MDPI Automation** — *fallback* | Published [S2]; fast, open access; the CNC framing this system implements is that paper's own | Lower prestige; APC |
| **Journal of Field Robotics** — *stretch* | Where [S1] appeared, and this work is a direct extension of its control layer. JFR also ran Adesiji et al., *Safety Considerations in Deployment of Robotic Systems — A Systematic Review*, 2026, doi:10.1002/rob.70022, so robot-safety methodology is live in that venue, not only field trials | JFR expects field deployment. Do not submit until `PB-SAFE-03`/`PB-SAFE-04` are closed **and** demonstrated on the physical arm — that is exactly what [S1] provided and what this system currently cannot |
| **Robotics & Computer-Integrated Manufacturing** | The head-as-workpiece framing is genuinely at home in Q1 manufacturing | Scope risk: haircutting may read as out of remit |

Recommendation: write for **IEEE JSAS** now, on the contract and determinism contribution, with the Tier B
table as an honest limitations section. Then close gaps 2 and 3 from §4, run them on hardware, and pursue
**JFR** as a follow-up that extends [S1] rather than restating it. The Tier A/Tier B split is directly
reusable as the paper's evaluation and limitations sections — that is what it was designed for.

There is a second, separate paper in the CAT/IRT assessment layer (`03-DYNAMIC-QBANK.md`,
`07-CALIBRATION.md`) aimed at robotics-education venues. It shares no evidence with this one and should not
be folded in.

## 7. Primary bibliography

Every entry is a primary result — an experiment, an algorithm, a measurement or a patent — and every one
was recovered from the reference list of [S2]. Records that also appear in [S3] use [S2]'s version, for
the reason given in §1.

> **Correction.** An earlier revision of this section claimed [S2]'s bibliography "checks out". It does
> not. **Eleven entries were checked against publishers; six were defective:** wrong year, volume and
> pages ([P16]); unresolvable venue ([P6]); two distinct papers merged into one record ([P4]); wrong year
> ([P18], [P19], [P20]). [S2] is a cleaner index than [S3], not a clean one — and [P2], a genuine primary,
> got [P5]'s pages wrong, so primacy is no guarantee either. [P3] was briefly counted as a seventh defect
> and is not one: [S2] cited a real, separately indexed conference version. **No entry below may be cited
> until it carries a ✅.**

Status key: **✅ resolved** against the publisher or arXiv during this review — venue, year, volume,
pages and identifier all confirmed. **⚠️ unresolved** — transcribed from [S2]'s PDF and *not* checked;
treat as a search lead, not a citation.

**Contact force and human-robot safety — the corpus's only quantitative safety source**

- **[P1]** ✅ Bilyea, A.J.N.; French, S.H.; Abdullah, H.A. *Modeling contact forces during human-robot interactions for performing activities of daily living.* Proc. Inst. Mech. Eng. Part H (Journal of Engineering in Medicine) 2023, 237(7), 829–840. doi:10.1177/09544119231179568; PMID 37300488; free full text at PMC10350725. — Measures contact force for **hair brushing**, face wiping and face shaving specifically. *PDF held locally.*

**Hair manipulation with real robots — the closest primaries to this system**

- **[P2]** ✅ Kim, S.; Kanazawa, N.; Hasegawa, S.; Kawaharazuka, K.; Okada, K. *Front Hair Styling Robot System Using Path Planning for Root-Centric Strand Adjustment.* IEEE/SICE Int. Symp. System Integration (SII) 2025, 544–549. doi:10.1109/SII59315.2025.10871088; arXiv:2501.10991. — *PDF held locally.*
- **[P3]** ✅ Hughes, J.; Plumb-Reyes, T.; Charles, N.; Mahadevan, L.; Rus, D. *Detangling Hair Using Feedback-Driven Robotic Brushing.* IEEE Robotics and Automation Letters 2021, 6(2), 3495–3502. Conference version: IEEE RoboSoft 2021, New Haven, 487–494, doi:10.1109/ROBOSOFT51838.2021.9479221 (IEEE Xplore document 9479221). — Both records are real; [S2] cited the conference one. Cite RA-L as the journal of record. The RA-L DOI was not confirmed in this review.
- **[P4]** ✅ Yoo, U.; Dennler, N.; Xing, E.; Matarić, M.; Nikolaidis, S.; Ichnowski, J.; Oh, J. *Soft and Compliant Contact-Rich Hair Manipulation and Care.* ACM/IEEE HRI 2025 (honorable mention); arXiv:2501.02630.
- **[P4a]** ✅ Yoo, U.; Dennler, N.; Matarić, M.; Nikolaidis, S.; Oh, J.; Ichnowski, J. *MOE-Hair: Toward Soft and Compliant Contact-Rich Hair Manipulation and Care.* HRI '24 Companion, 1163–1167. doi:10.1145/3610978.3640682 — the earlier late-breaking report. **[S2] merged these two into one record**; they are separate papers a year apart.
- **[P5]** ✅ Dennler, N.; Shin, E.; Matarić, M.; Nikolaidis, S. *Design and Evaluation of a Hair Combing System Using a General-Purpose Robotic Arm.* IEEE/RSJ IROS 2021, Prague, **3739–3746**; arXiv:2108.01233. — [S2] was right here and **[P2]'s bibliography was wrong** (it gives 1234–1241). Primary sources are not automatically clean either.
- **[P6]** ⚠️ **Venue unresolvable — do not cite without checking IEEE Xplore by hand.** Bai, Y.; Seita, D.; Edsinger, A.; Goldberg, K. *RoboWig: A Hair Brushing Robot with Visual and Tactile Sensing.* [S2] says IEEE/RSJ IROS 2022. [S4] says "ICRA, Chennai, India, 28–29 January 2022, pp. 5781–5787" — **which cannot be true:** ICRA 2022 was Philadelphia, 23–27 May; IROS 2022 was Kyoto, 23–27 October. Neither met in Chennai in January. Search engines now echo [S4]'s version back as fact, so the corrupt record is self-reinforcing. This is what citation laundering looks like in practice.

**Hair modeling**

- **[P7]** ⚠️ Selle, A.; Lentine, M.; Fedkiw, R. *A mass spring model for hair simulation.* ACM SIGGRAPH 2008.
- **[P8]** ⚠️ Sklyarova, V.; Chelishev, J.; Dogaru, A.; Medvedev, I.; Lempitsky, V.; Zakharov, E. *Neural Haircut: prior-guided strand-based hair reconstruction.* ICCV 2023, 19762–19773.
- **[P9]** ⚠️ Zhou, Y. et al. *HairNet: single-view hair reconstruction using convolutional neural networks.* ECCV 2018, 235–251.
- **[P10]** ⚠️ Shen, Y. et al. *CT2Hair: high-fidelity 3D hair modeling using computed tomography.* ACM TOG 2023, 42.
- **[P11]** ⚠️ Zhou, Y.; Chai, M.; Pepe, A.; Gross, M.; Beeler, T. *GroomGen: a high-quality generative hair model using hierarchical latent representations.* ACM TOG 2023, 42(6).

**Head pose and disturbance compensation — the moving workpiece**

- **[P12]** ⚠️ Chun, S.; Chang, J.Y. *6DoF Head Pose Estimation through Explicit Bidirectional Interaction with Face Geometry.* ECCV 2024, 146–163.
- **[P13]** ⚠️ Meyer, G.P.; Gupta, S.; Frosio, I.; Reddy, D.; Kautz, J. *Robust model-based 3D head pose estimation.* ICCV 2015, 3649–3657.
- **[P14]** ⚠️ Du, C.; Li, H.; Thum, C.; Lewis, F.; Wang, Y. *Simple disturbance observer for disturbance compensation.* IET Control Theory Appl. 2010, 4, 1748–1755.

**Supervised autonomy and human-in-the-loop control — the basis for `PB-HRI-01`**

- **[P15]** ✅ Dass, S.; Pertsch, K.; Zhang, H.; Lee, Y.; Lim, J.J.; Nikolaidis, S. *PATO: Policy Assisted TeleOperation for Scalable Robot Data Collection.* **Robotics: Science and Systems (RSS) 2023**; arXiv:2212.04708, doi:10.48550/arXiv.2212.04708. — **Acronym collision:** searching "PATO" returns *PATO: Pangenome Analysis Toolkit* (Bioinformatics 2021, 37(23), 4564–4566, doi:10.1093/bioinformatics/btab697), an unrelated R package. Always search by the arXiv ID.
- **[P16]** ✅ Liu, H.; Nasiriany, S.; Zhang, L.; Bao, Z.; Zhu, Y. *Robot Learning on the Job: Human-in-the-Loop Autonomy and Learning During Deployment.* Int. J. Robotics Research **2025, 44(10–11), 1727–1742**. doi:10.1177/02783649241273901; arXiv:2211.08416. — **[S2] cited this as "2024, 5, 83–96": wrong year, wrong volume, wrong pages.** "vol. 5, 83–96" collides with [P17] (Khatib, IJRR 1986, **5**, 90–98) directly below it in [S2]'s list — the same neighbour-contamination pattern flagged for [S3] in §1.

**Obstacle avoidance and redundancy resolution — the bridge to the ZNN line**

- **[P17]** ⚠️ Khatib, O. *Real-time obstacle avoidance for manipulators and mobile robots.* Int. J. Robotics Research 1986, 5(1), 90–98.
- **[P18]** ✅ Khan, A.H.; Li, S.; Luo, X. *Obstacle Avoidance and Tracking Control of Redundant Robotic Manipulator: An RNN-Based Metaheuristic Approach.* IEEE Trans. Industrial Informatics **2020**, 16(7), 4670–4680. doi:10.1109/TII.2019.2941916 — [S2] dated it 2019; the `TII.2019` DOI stem is early access, the issue is 2020.
- **[P19]** ✅ Xu, Z.; Zhou, X.; Wu, H.; Li, X.; Li, S. *Motion Planning of Manipulators for Simultaneous Obstacle Avoidance and Target Tracking: An RNN Approach With Guaranteed Performance.* IEEE Trans. Industrial Electronics **2022**, 69(4), 3887–3897. doi:10.1109/TIE.2021.3073305 — [S2] dated it 2021.
- **[P20]** ✅ Li, X.; Xu, Z.; Li, S.; Su, Z.; Zhou, X. *Simultaneous obstacle avoidance and target tracking of multiple wheeled mobile robots with certified safety.* IEEE Trans. Cybernetics **2022**, 52(11), 11859–11873. — [S2] dated it 2021.

[P18]–[P20] are Shuai Li's own control line, cited by [S2] as the safety and obstacle-avoidance backing for
robotic haircutting. That is the author drawing the bridge himself: his RNN redundancy-resolution work is
the intended control substrate for this application. It is the natural comparison baseline if an
alternative IK backend is ever built for `hcr_sim`.

**Teleoperation under delay**

- **[P21]** ⚠️ Anderson, R.J.; Spong, M.W. *Bilateral control of teleoperators with time delay.* IEEE SMC 1988, 1, 131–138.
- **[P22]** ⚠️ Niemeyer, G.; Slotine, J.J.E. *Telemanipulation with time delays.* Int. J. Robotics Research 2004, 23, 873–890.
- **[P23]** ⚠️ Lee, D.; Spong, M.W. *Passive bilateral teleoperation with constant time delay.* IEEE Trans. Robotics 2006, 22, 269–281.
- **[P24]** ⚠️ Nuño, E.; Basañez, L.; Ortega, R. *Passivity-based control for bilateral teleoperation: a tutorial.* Automatica 2011, 47, 485–495.

**Machining framing — both reviews, see the §2 gap note**

- **[P25]** ⚠️ Lasemi, A.; Xue, D.; Gu, P. *Recent development in CNC machining of freeform surfaces: a state-of-the-art review.* Computer-Aided Design 2010, 42, 641–654.
- **[P26]** ⚠️ Makhanov, S.S. *Vector fields for five-axis machining: a survey.* Int. J. Adv. Manuf. Technol. 2022, 122, 533–575.

**Biomechanical and thermal injury limits — recovered from [S1]'s bibliography, and the real source of its numbers**

- **[P29]** ✅ Behrens, R.; Pliske, G.; Umbreit, M.; Piatek, S.; Walcher, F.; Elkmann, N. *A Statistical Model to Determine Biomechanical Limits for Physically Safe Interactions With Collaborative Robots.* Frontiers in Robotics and AI 2022, 8:667818. doi:10.3389/frobt.2021.667818; free at PMC8850785. — Fraunhofer IFF, the group behind the ISO/TS 15066 pain-threshold work. **This, not [S1], is where `PB-SAFE-05`'s number comes from.**
- **[P30]** ✅ Martin, N.; Falder, S. *A review of the evidence for threshold of burn injury.* Burns 2017, 43(8), 1624–1639. — Clinical source of the 43–45 °C limit. Relevant only once a heated tool exists; recorded now so it is not re-derived later.
- **[P31]** ⚠️ Kirschner, R.J.; Micheler, C.M.; Zhou, Y.; et al. *Towards safe robot use with edged or pointed objects: A surrogate study assembling a human hand injury protection database.* IEEE 2024, 12680–12687 (ICRA 2024; venue not confirmed). — **Injury data for robots holding sharp tools.** The closest thing in any of these bibliographies to the actual hazard this system presents. Not cited by [S2]–[S6]; found only in [S1].
- **[P32]** ⚠️ Selvaggio, M.; Cognetti, M.; Nikolaidis, S.; Ivaldi, S.; Siciliano, B. *Autonomy in physical human-robot interaction: A brief survey.* IEEE Robotics and Automation Letters 2021, 6(4), 7989–7996. — A survey, so it establishes no criterion, but it is the standard framing reference for the autonomy spectrum `PB-HRI-01` sits on.

**Standards — normative, not scholarly, and the hook for the JSAS "standards" remit**

- **[N1]** ISO/TS 15066:2016, *Robots and robotic devices — Collaborative robots.* Annex A carries the per-body-region force and pressure limits. **Required reading before `PB-SAFE-05`.**
- **[N2]** ISO 13482:2014, *Safety requirements for personal care robots.* The category a haircutting robot actually falls into.
- **[N3]** ISO 10218-1:2025 / ISO 10218-2:2025, *Robotics — Safety requirements.* [S1] §7 states explicitly that these "do not cover the unique requirements of haircutting robots" — quote that when positioning the contribution.

**Prior art**

- **[P27]** ⚠️ Gronier, J. *Hair-Cutting Machine.* U.S. Patent 3,241,562, 22 March 1966.
- **[P28]** ⚠️ Aladabbah, M. *Automatic Hair Cutter Robot.* WO 2023/080812 A1, 11 May 2023.

### What to obtain next, in order

Every row resolves by identifier. Open-access column is a route that needs no subscription.

| # | Ref | Why | Resolve by | Open access |
| --- | --- | --- | --- | --- |
| 1 | **[S1]** *Safety in Robotic Haircutting*, J. Field Robotics 2026 | Source of the §7 gap statement §6 now builds on. Survey, not primary — *preprint held locally* | <https://doi.org/10.1002/rob.70305> | Authorea preprint <https://doi.org/10.22541/au.175917093.39470494/v1> |
| 2 | **[P16]** Liu et al., IJRR 2025 | Basis for the `PB-HRI-01` deviation monitor | <https://doi.org/10.1177/02783649241273901> | <https://arxiv.org/abs/2211.08416> |
| 3 | **[P15]** PATO, RSS 2023 | Same criterion, different mechanism | <https://doi.org/10.48550/arXiv.2212.04708> | <https://arxiv.org/abs/2212.04708> |
| 4 | **[P1]** Bilyea et al. 2023 | The corpus's only measured contact forces, and it measures **hair brushing** | <https://doi.org/10.1177/09544119231179568> | <https://pmc.ncbi.nlm.nih.gov/articles/PMC10350725/> — *held locally* |
| 5 | **[P3]** Hughes et al., RA-L 2021 | Only primary with a validated physical hair-contact model | <https://doi.org/10.1109/ROBOSOFT51838.2021.9479221> (conference version) | — |
| 6 | **[P4]** Yoo et al., HRI 2025 | Soft compliant contact-rich hair care | <https://dl.acm.org/doi/10.5555/3721488.3721564> | <https://arxiv.org/abs/2501.02630> |
| 7 | **[P4a]** MOE-Hair, HRI '24 Companion | The earlier LBR [S2] merged into the above | <https://doi.org/10.1145/3610978.3640682> | — |
| 8 | **[P5]** Dennler et al., IROS 2021 | Real-robot combing system | — | <https://arxiv.org/abs/2108.01233> |
| 9 | **[P2]** Kim et al., SII 2025 | Closest primary to this system | <https://doi.org/10.1109/SII59315.2025.10871088> | <https://arxiv.org/abs/2501.10991> — *held locally* |
| 10 | **[P19]** Xu, …, Li, TIE 2022 | Shuai Li's RNN line, the bridge [S2] draws itself | <https://doi.org/10.1109/TIE.2021.3073305> | — |
| 11 | **[P18]** Khan, Li & Luo, TII 2020 | Same line | <https://doi.org/10.1109/TII.2019.2941916> | — |
| — | Everything still marked ⚠️ | Resolve before any of it reaches a manuscript | — | — |

**All eleven rows above are now held locally as PDFs**, plus the [S1] preprint. The published IEEE
versions of [P2] (SII 2025, Munich, 21–24 January 2025) and [P3] (RoboSoft 2021, Yale, 12–16 April 2021)
are held alongside the arXiv preprints; the SII front matter independently confirms [P2]'s venue and the
RoboSoft front matter confirms that [S2]'s conference citation for [P3] was legitimate.

Still to obtain: **[N1]** ISO/TS 15066 (paid standard), **[P29]** (free at PMC8850785), **[P31]**.

### Search hygiene

Four separate attempts to obtain papers from this list landed on the wrong work, every time because a
venue or system was named by acronym alone:

| Searched | Landed on | Wanted |
| --- | --- | --- |
| "JFR" | Journées Francophones de Radiologie | Journal of Field Robotics |
| "PATO" | *PATO: Pangenome Analysis Toolkit*, Bioinformatics 2021 | [P15], RSS 2023 |
| "IJRR 2024" | *Multimotion Visual Odometry*, Judd & Gammell | [P16], IJRR 2025 |
| "RoboSoft" | RoboSoft **manipulation challenge** soft-gripper papers, e.g. Front. Robot. AI 11:1356692 | [P3], IEEE RoboSoft **conference** |

**Resolve by identifier, never by acronym.** Every entry above therefore carries a DOI or an arXiv ID
where one exists; if an entry has neither, that is itself the reason it is still ⚠️.
