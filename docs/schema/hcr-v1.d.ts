/**
 * hcr.v1 — normative wire schema.
 *
 * This file is the single source of truth for the frontend/backend contract.
 * It is intentionally self-contained (no imports) so it can be copied into either
 * side of the wire without path juggling. It lives under docs/ and is NOT part of
 * the app's tsconfig include set, so it never affects the v1 build.
 *
 * Section 1 types are COPIED VERBATIM from the v1 app and are frozen by
 * SPEC v0.3 §15. If they ever diverge from src/, src/ is correct and this file
 * is a bug.
 *   - src/types/domain.ts
 *   - src/features/blockly/programTypes.ts
 *
 * Rust mirror: ./hcr_v1.rs
 */

// ---------------------------------------------------------------------------
// 1. Frozen v1 domain types (mirrors of src/, do not edit independently)
// ---------------------------------------------------------------------------

export type JointId = string;
export type VoxelKey = `${number},${number},${number}`;
export type Axis = 'x' | 'y' | 'z';
export type Vec3Tuple = readonly [number, number, number];
export type AllowedBlockType = 'set-joint-angle' | 'wait' | 'repeat';

export interface VoxelCoord { x: number; y: number; z: number }

export interface JointConfig {
  id: JointId;
  name: string;
  axis: Axis;
  minAngleDeg: number;
  maxAngleDeg: number;
  initialAngleDeg: number;
  speedDegPerSec: number;
}

export interface RobotCollisionConfig {
  linkRadius: number;
  jointRadius: number;
  toolShaftRadius: number;
  headClearance: number;
}

export interface RobotGeometryConfig {
  basePosition: Vec3Tuple;
  shoulderHeight: number;
  upperArmLength: number;
  forearmLength: number;
  toolLength: number;
  toolRadius: number;
  collision: RobotCollisionConfig;
}

export interface HairstyleDefinition { id: string; name: string; voxels: VoxelCoord[] }

export interface ScoreWeights { completion: number; efficiency: number; time: number }

export interface ScoringConfig {
  weights: ScoreWeights;
  referenceProgramCost: number;
  referenceTimeMs: number;
  commandWeight: number;
}

export interface ProgramMetrics {
  sourceBlockCount: number;
  executedCommandCount: number;
  estimatedDurationMs: number;
}

export interface ChallengeSummary { id: string; name: string; description: string }

export interface ChallengeDefinition {
  id: string;
  name: string;
  description: string;
  robotConfig: { joints: JointConfig[]; geometry: RobotGeometryConfig };
  voxelConfig: { origin: Vec3Tuple; size: number; headCenter: Vec3Tuple; headScale: Vec3Tuple };
  initialHair: HairstyleDefinition;
  targetHair: HairstyleDefinition;
  allowedBlocks: AllowedBlockType[];
  starterWorkspace: Record<string, unknown>;
  scoring: ScoringConfig;
}

export interface ScoreResult {
  completionScore: number;
  efficiencyScore: number;
  timeScore: number;
  finalScore: number;
  programCost: number;
}

/** Program IR — the contract's centre of gravity. Three executors consume this. */
export type RobotCommand =
  | { type: 'set-joint-angle'; jointId: JointId; angleDeg: number; sourceBlockId: string }
  | { type: 'wait'; durationMs: number; sourceBlockId: string };

export type ProgramNode =
  | RobotCommand
  | { type: 'repeat'; count: number; body: ProgramNode[]; sourceBlockId: string };

export interface Program { nodes: ProgramNode[]; sourceBlockCount: number }

// ---------------------------------------------------------------------------
// 2. Envelope
// ---------------------------------------------------------------------------

export interface ActorRef { type: 'user' | 'device' | 'service'; id: string }

export interface Envelope<K extends string = string, P = unknown> {
  v: 1;
  /** ULID. Unique per message; doubles as the idempotency key. */
  id: string;
  kind: K;
  /** Sender epoch-ms. Informational ONLY — never an ordering key. */
  ts: number;
  /** Echoes the `id` of the request this answers. */
  corr?: string;
  /** Topic the responder must publish the reply to. Must be inside the caller's own subtree. */
  replyTo?: string;
  src?: ActorRef;
  payload: P;
}

export interface HcrError {
  code: HcrErrorCode;
  message: string;
  retryable: boolean;
  /** Field path for validation errors; enables Blockly block highlighting. */
  field?: string;
  details?: Record<string, unknown>;
}

export type HcrErrorCode =
  | 'UNAUTHORIZED' | 'FORBIDDEN'
  | 'CHALLENGE_NOT_FOUND'
  | 'PROGRAM_INVALID' | 'PROGRAM_TOO_LARGE' | 'WEIGHTS_INVALID'
  | 'ITEM_REF_INVALID'
  | 'SESSION_NOT_FOUND' | 'SESSION_TERMINATED' | 'BANK_EXHAUSTED'
  | 'MATCH_NOT_READY'
  /** A Cutter Grid trajectory failed verification. `details.rejection` names which audit. */
  | 'TRAJECTORY_REJECTED'
  /** A valid V4 Cutter Grid program could not be planned. `details.plannerCode` names the stage failure. */
  | 'TRAJECTORY_PLANNING_FAILED'
  | 'DEVICE_OFFLINE' | 'DEVICE_BUSY'
  | 'REPLAY_TIMEOUT' | 'RATE_LIMITED' | 'INTERNAL';

/**
 * Which audit refused a Cutter Grid trajectory, carried in `HcrError.details.rejection`.
 * Not separate error codes: thirteen would be thirteen things for every client to learn.
 */
export type CutterGridRejection =
  | 'UNSUPPORTED_PLAN_VERSION' | 'SIGNATURE_MISMATCH' | 'STEP_MISMATCH'
  | 'COORD_DISCONTINUITY' | 'JOINT_LIMIT' | 'HEAD_COLLISION'
  | 'POSE_DISCONTINUITY' | 'END_EFFECTOR_MISMATCH' | 'AXIS_DISPLACEMENT'
  | 'PATH_DEVIATION' | 'TIMELINE_INVALID' | 'ENTRY_CUTS_HAIR' | 'TOO_MANY_WAYPOINTS';

// ---------------------------------------------------------------------------
// 3. Catalog
// ---------------------------------------------------------------------------

/** IRT item parameters. `c` is ~0 for HCR tasks — see 03-DYNAMIC-QBANK.md §2. */
export interface ItemParameters {
  discrimination: number;   // a
  difficulty: number;       // b, logit scale, validated to [-3, 3]
  guessing: number;         // c
}

export type CalibrationState = 'provisional' | 'online' | 'calibrated' | 'retired';

export type SkillDimension =
  | 'kinematics' | 'sequencing' | 'iteration' | 'precision' | 'safety';

export interface ChallengeMeta {
  version: number;
  irt: ItemParameters;
  calibration: CalibrationState;
  /** Responses observed for this item version; drives calibration promotion. */
  responseCount: number;
  dimensions: SkillDimension[];
  /** Mastery threshold on normalized final score, default 0.5. See 03 §2. */
  masteryThreshold: number;
  /** Present when produced by an item family generator. */
  generator?: { familyId: string; seed: number; params: Record<string, number>; version: string };
  /** Whether this challenge can run on physical hardware (shoulderRoll constraint). */
  hardwareCompatible: boolean;
  /**
   * Editors this item can be attempted in. Defaults to `['servo']` when absent.
   *
   * Cutter Grid needs a certified planner profile per challenge — a proof that the lattice is
   * reachable and that a reference program achieves the target — so most items are servo-only.
   */
  programmingModes?: ProgrammingMode[];
}

export interface ChallengeDefinitionDto extends ChallengeDefinition {
  meta: ChallengeMeta;
}

// ---------------------------------------------------------------------------
// 4. Submission & authoritative scoring
// ---------------------------------------------------------------------------

export interface ClientPreview {
  scoreResult: ScoreResult;
  /** Hash over the SORTED result voxel key list. See 02-DETERMINISM.md §5. */
  resultVoxelsHash: string;
  engineVersion: string;
  tickMs: number;
}

export interface SubmissionCreate {
  /** Client-generated ULID. Idempotency key — resubmitting returns the first result. */
  submissionId: string;
  challengeId: string;
  challengeVersion: number;
  /** Program IR nodes. NEVER runtimeCommands — the server expands `repeat` itself. */
  program: Program;
  /**
   * Set when the program was written in Cutter Grid rather than with joint angles.
   *
   * Carries the lattice IR *and* the frozen trajectory, because a Cutter Grid motion is not
   * derivable from its program without redoing the browser's compile-time IK search. When
   * present the server verifies the trajectory and `program` is empty. See `08-CUTTER-GRID.md`.
   */
  cutterGrid?: CutterGridSubmission;
  sessionId?: string;
  itemRef?: string;
  /** Set when submitting into a competitive round; acceptance is by server receive time. */
  matchId?: string;
  clientPreview?: ClientPreview;
}

// --- Cutter Grid -----------------------------------------------------------
//
// Mirrors `src/features/cutter-grid/types.ts`. A separate, additive family: the frozen v1
// `Program` and `RobotCommand` are untouched, as SPEC v0.3 §15.4 requires.

/**
 * Which editor a program was written in. Not a rendering detail: one servo command drives a
 * joint, one Cutter Grid command crosses a lattice cell, so the same challenge is a different
 * task with a different difficulty in each. `servo` is the reference and is what an absent
 * value means everywhere this is optional.
 */
export type ProgrammingMode = 'servo' | 'cutter-grid';

export type CutterGridDirection =
  | 'right' | 'left' | 'up' | 'down' | 'forward' | 'backward';

/** Logical lattice coordinate. `[0,0,0]` is where the certified entry pose puts the tool. */
export type CutterGridCoord = [number, number, number];

export type CutterGridNode =
  | { type: 'move'; direction: CutterGridDirection; distance: number; sourceBlockId: string }
  | { type: 'wait'; durationMs: number; sourceBlockId: string }
  | { type: 'repeat'; count: number; body: CutterGridNode[]; sourceBlockId: string };

export interface CutterGridProgram {
  kind: 'cutter-grid';
  version: 1;
  plannerVersion: string;
  nodes: CutterGridNode[];
  sourceBlockCount: number;
}

export interface CutterTrajectoryWaypoint {
  timeMs: number;
  /** Servo degrees per joint. */
  jointAngles: Record<string, number>;
  /** Playback only; the server samples at waypoints and does not read this. */
  jointVelocitiesDegPerSec?: Record<string, number>;
  /** Checked against forward kinematics, never trusted. */
  endEffector: Vec3Tuple;
}

export interface CutterTrajectoryStep {
  index: number;
  kind: 'move-cell' | 'wait';
  sourceBlockId: string;
  startCoord: CutterGridCoord;
  endCoord: CutterGridCoord;
  durationMs: number;
  waypoints: CutterTrajectoryWaypoint[];
  /** Advisory — the server carves from its own sweep and only compares. */
  expectedCutVoxels?: VoxelKey[];
}

export interface CutterGridPlanningDiagnostics {
  entryOptionId: string;
  cartesianLayerCount: number;
  candidateCounts?: number[];
  seedBudgetUsed: number;
  minimumHeadClearance: number;
  minimumJointLimitMargin: number;
  maximumNormalizedJointStep: number;
}

export interface CutterTrajectoryPlan {
  kind: 'cutter-grid-trajectory';
  version: 2;
  plannerVersion: string;
  /** fnv1a64 over the challenge; the server recomputes it and refuses a mismatch. */
  challengeSignature: string;
  entryOptionId: string;
  /** Rest pose to lattice origin. Cuts nothing, costs no commands, charged no time. */
  positioningTrajectory?: CutterTrajectoryWaypoint[];
  startCoord: CutterGridCoord;
  endCoord: CutterGridCoord;
  steps: CutterTrajectoryStep[];
  /** Advisory; used only for divergence telemetry. */
  expectedResultVoxels?: VoxelKey[];
  estimatedDurationMs: number;
  /** Re-derived server-side and compared, never substituted for the server's count. */
  executedCommandCount: number;
  diagnostics: CutterGridPlanningDiagnostics;
  /** fnv1a64 over the plan without this field. Integrity, not authenticity. */
  trajectorySignature: string;
}

export interface CutterGridSubmission {
  program: CutterGridProgram;
  plan: CutterTrajectoryPlan;
}

// --- Cutter Grid V4 server planning ---------------------------------------
//
// V2 above remains a read-only, client-uploaded trajectory compatibility path.
// These V4 types are deliberately independent: the browser sends only the
// program and Challenge reference; the server owns the certified Profile and
// returns the compact PTP plan it created.

export type CutterGridProgramV4 = CutterGridProgram & {
  plannerVersion: 'cutter-grid-compact-ptp-v4';
};

export interface CutterGridBoundsV4 {
  min: CutterGridCoord;
  max: CutterGridCoord;
}

export type CutterGridStaticIkStatusV4 =
  | 'safe-candidate-known'
  | 'no-safe-candidate-found';

export interface CutterGridNodeProfileV4 {
  coord: CutterGridCoord;
  worldPosition: Vec3Tuple;
  staticIkStatus: CutterGridStaticIkStatusV4;
  candidateCount: number;
  seedBudget: number;
}

export interface CutterTrajectoryBoundaryStateV4 {
  jointAngles: Record<JointId, number>;
  jointVelocitiesDegPerSec: Record<JointId, number>;
  jointAccelerationsDegPerSec2: Record<JointId, number>;
}

export interface CutterGridSyncPtpPrimitiveV4 {
  kind: 'sync-ptp';
  interpolation: 'synchronized-quintic';
  durationMs: number;
  start: CutterTrajectoryBoundaryStateV4;
  end: CutterTrajectoryBoundaryStateV4;
}

export interface CutterGridContactEventV4 {
  timeMs: number;
  voxelKeys: VoxelKey[];
}

export type CutterGridTrajectoryActionV4 =
  | {
      type: 'move';
      occurrenceId: string;
      sourceBlockId: string;
      direction: CutterGridDirection;
      distance: number;
      startCoord: CutterGridCoord;
      endCoord: CutterGridCoord;
      logicalCommandCount: number;
      /** Exactly one direct primitive, or one detour waypoint expressed as two primitives. */
      primitives: [CutterGridSyncPtpPrimitiveV4] | [CutterGridSyncPtpPrimitiveV4, CutterGridSyncPtpPrimitiveV4];
      contactEvents: CutterGridContactEventV4[];
      expectedCutVoxels: VoxelKey[];
    }
  | {
      type: 'wait';
      occurrenceId: string;
      sourceBlockId: string;
      durationMs: number;
      logicalCommandCount: 1;
      expectedCutVoxels: [];
    };

export interface CutterGridPositioningPlanV4 {
  entryOptionId: string;
  primitives: CutterGridSyncPtpPrimitiveV4[];
  trajectorySignature: string;
}

export interface CutterGridJointMotionLimitsV4 {
  nominalVelocityDegPerSec: number;
  nominalAccelerationDegPerSec2: number;
  nominalJerkDegPerSec3: number;
  maxVelocityDegPerSec: number;
  maxAccelerationDegPerSec2: number;
  maxJerkDegPerSec3: number;
}

export interface CutterGridMotionLimitsV4 {
  requestedSpeedScale: number;
  joints: Record<JointId, CutterGridJointMotionLimitsV4>;
}

export interface CutterGridPlanningDiagnosticsV4 {
  endpointLayerCount: number;
  candidateCounts: number[];
  expandedActionIndex?: number;
  directPrimitiveCount: number;
  detourPrimitiveCount: number;
  minimumHeadClearance: number;
  minimumJointLimitMargin: number;
  maximumNormalizedJointStep: number;
  maximumEndEffectorChordDeviation: number;
  requestedSpeedScale: number;
  actualSpeedScale: number;
  maximumVelocityRatio: number;
  maximumAccelerationRatio: number;
  maximumJerkRatio: number;
  adaptiveValidationSampleCount: number;
}

export interface CutterTrajectoryPlanV4 {
  kind: 'cutter-grid-trajectory';
  version: 4;
  plannerVersion: 'cutter-grid-compact-ptp-v4';
  challengeSignature: string;
  positioning: CutterGridPositioningPlanV4;
  startCoord: CutterGridCoord;
  endCoord: CutterGridCoord;
  actions: CutterGridTrajectoryActionV4[];
  expectedResultVoxels: VoxelKey[];
  estimatedDurationMs: number;
  executedCommandCount: number;
  motionLimits: CutterGridMotionLimitsV4;
  motionLimitsSignature: string;
  diagnostics: CutterGridPlanningDiagnosticsV4;
  trajectorySignature: string;
}

export interface CutterGridEntryOptionV4 {
  id: string;
  jointAngles: Record<JointId, number>;
  positioningPrimitive: CutterGridSyncPtpPrimitiveV4;
  positioningSignature: string;
  minimumHeadClearance: number;
}

export interface CutterGridRoadmapNodeV4 {
  id: string;
  jointAngles: Record<JointId, number>;
  minimumHeadClearance: number;
}

export interface CutterGridRoadmapEdgeV4 {
  fromNodeId: string;
  toNodeId: string;
}

export interface CutterGridRoadmapV4 {
  nodes: CutterGridRoadmapNodeV4[];
  edges: CutterGridRoadmapEdgeV4[];
  signature: string;
}

export interface CutterGridCertificationV4 {
  passed: boolean;
  entryZeroContact: boolean;
  referenceCompletion: number;
  referenceCutVoxels: VoxelKey[];
  referenceExtraCutVoxels: VoxelKey[];
  certifiedDirections: CutterGridDirection[];
  authenticatedEntryOptionIds: string[];
  referenceTrajectoryCertified: boolean;
}

/** Server-owned; it is a startup/CI asset and is never accepted from a client. */
export interface CutterGridProfileV4 {
  version: 4;
  plannerVersion: 'cutter-grid-compact-ptp-v4';
  challengeSignature: string;
  originHairCoord: CutterGridCoord;
  originWorldPosition: Vec3Tuple;
  bounds: CutterGridBoundsV4;
  entryOptions: CutterGridEntryOptionV4[];
  nodes: CutterGridNodeProfileV4[];
  referenceProgram: CutterGridProgramV4;
  referenceTrajectorySignature: string;
  certification: CutterGridCertificationV4;
  motionLimits: CutterGridMotionLimitsV4;
  motionLimitsSignature: string;
  roadmap: CutterGridRoadmapV4;
  profileSignature: string;
}

export type CutterGridPlanningErrorCodeV4 =
  | 'planner-not-ready' | 'planning-cancelled' | 'profile-v4-mismatch' | 'out-of-bounds'
  | 'endpoint-ik-not-converged' | 'endpoint-ik-search-exhausted'
  | 'endpoint-ptp-disconnected' | 'motion-primitive-budget-exhausted'
  | 'ptp-collision' | 'ptp-certificate-failed'
  | 'actual-sweep-certification-failed' | 'plan-signature-mismatch';

export type CutterGridPlanningStageV4 =
  | 'profile' | 'endpoint' | 'ptp-edge' | 'roadmap'
  | 'motion-certificate' | 'sweep-certificate' | 'serialization';

/** HTTP `POST /api/v1/cutter-grid/plans` request. The client never uploads a Profile. */
export interface CutterGridPlanRequestV1 {
  challengeId: string;
  challengeVersion: number;
  program: CutterGridProgramV4;
}

/** Successful HTTP V4 planning response. `planningDurationMs` is observability, not plan identity. */
export interface CutterGridPlanResponseV1 {
  kind: 'cutter-grid-plan-result';
  version: 1;
  plannerImplementation: 'hcr-sim-rust';
  plannerBuild: string;
  profileSignature: string;
  planningDurationMs: number;
  plan: CutterTrajectoryPlanV4;
}

export interface SubmissionAccepted { submissionId: string; state: 'queued' }

export type TerminalReason =
  | 'completed' | 'head-collision' | 'command-limit' | 'invalid' | 'timeout';

export interface SubmissionResult {
  submissionId: string;
  challengeId: string;
  challengeVersion: number;
  status: 'completed' | 'error';
  /** Which engine produced this score. Absent means `servo`. */
  programmingMode?: ProgrammingMode;
  /** Authoritative. Produced by server replay, not by the client. */
  score: ScoreResult;
  metrics: ProgramMetrics;
  resultVoxelsHash: string;
  terminal: {
    reason: TerminalReason;
    jointId?: JointId;
    safeAngleDeg?: number;
    sourceBlockId?: string;
    partLabel?: string;
  };
  replay: {
    engineVersion: string;
    tickMs: number;
    simulatedMs: number;
    /** True when clientPreview disagreed — a conformance alarm, not a user-facing error. */
    divergedFromClient: boolean;
  };
  error?: HcrError;
}

// ---------------------------------------------------------------------------
// 5. Adaptive session (CAT)
// ---------------------------------------------------------------------------

export interface SessionSnapshot {
  sessionId: string;
  /** Ability estimate θ, logit scale. */
  theta: number;
  /** Standard error of θ. Infinity before the first response. */
  standardError: number;
  responseCount: number;
  expectedRemaining: number | null;
  state: 'active' | 'awaiting-response' | 'terminated' | 'finalized';
  terminationReason?: string;
}

export interface SessionStart {
  /**
   * Which editor this session is practised in. Absent means `servo`.
   *
   * A session runs in one mode throughout and its ability estimate belongs to that mode alone,
   * mirroring the rule that match play never touches θ_solo. `initialTheta` must come from the
   * same mode. See 07-CALIBRATION.md §11.
   */
  programmingMode?: ProgrammingMode; blueprintId?: string; initialTheta?: number }

export interface NextItem {
  /** Opaque HMAC token binding (sessionId, bankIndex, challengeId, version, issuedAt). */
  itemRef: string;
  challengeId: string;
  challengeVersion: number;
  expectedRemaining: number | null;
}

export interface SessionRespond { sessionId: string; itemRef: string; submissionId: string }

export interface ResponseOutcome {
  /** arona's dichotomized verdict: remapped score > 0.5. See 03 §2. */
  correct: boolean;
  /** Raw normalized score in [0,1], preserved for future polytomous models. */
  rawScore: number;
  theta: number;
  standardError: number;
  terminated: boolean;
  terminationReason?: string;
}

export interface SessionResultDto {
  sessionId: string;
  finalTheta: number;
  standardError: number;
  totalItems: number;
  durationMs: number;
  terminationReason: string;
  /** Per-dimension breakdown when the blueprint tracks dimensions. */
  dimensionScores?: Partial<Record<SkillDimension, number>>;
  items: Array<{
    challengeId: string;
    challengeVersion: number;
    rawScore: number;
    correct: boolean;
    thetaBefore: number;
    thetaAfter: number;
    responseTimeMs: number;
  }>;
}

// ---------------------------------------------------------------------------
// 6. Device
// ---------------------------------------------------------------------------

/** Physical servo axis. The ESP8266 kit exposes X, Y, Z, B, E (and a spare T). */
export type AxisId = 'X' | 'Y' | 'Z' | 'B' | 'E' | 'T';

export interface AxisConfig {
  axis: AxisId;
  /** Simulator joint this axis realizes; null for hardware-only axes such as the gripper. */
  jointId: JointId | null;
  minDeg: number;          // servo domain, 0..180 on this kit
  maxDeg: number;
  homeDeg: number;
  /** servoDeg = clamp(centerDeg + direction * (jointDeg - offsetDeg), minDeg, maxDeg) */
  centerDeg: number;
  direction: 1 | -1;
  offsetDeg: number;
  /** Measured, not assumed. Drives both motion timing and the simulator's speed model. */
  speedDegPerSec: number;
}

export interface AxisState { axis: AxisId; angleDeg: number }

export type DeviceStatus =
  | 'offline' | 'idle' | 'running' | 'paused' | 'faulted' | 'estopped' | 'uncalibrated';

export interface DeviceState {
  status: DeviceStatus;
  firmware: string;
  /** How this device wants its payloads encoded. Set by the device, honoured by the backend. */
  wireFormat: 'json' | 'cbor' | 'text';
  axes: AxisState[];
  queuedCommands: number;
  rssi?: number;
  fault?: HcrError;
}

export interface DeviceTelemetry {
  /** Device-monotonic milliseconds since boot. Backend stamps arrival separately. */
  tMono: number;
  angles: number[];
  busy: boolean;
}

export interface DeviceCommand {
  corr: string;
  op: 'run' | 'home' | 'stop' | 'resume' | 'query';
  /** Present for op === 'run'. Same IR the browser and server executed. */
  program?: Program;
  /** Pre-translated text for kit firmware that speaks the seller's command language. */
  textCmd?: string;
}

export interface DeviceEvent {
  event: 'cmd.start' | 'cmd.done' | 'fault' | 'limit';
  corr?: string;
  sourceBlockId?: string;
  axis?: AxisId;
  detail?: string;
}

export interface DeviceAck { corr: string; ok: boolean; error?: HcrError }

// ---------------------------------------------------------------------------
// 6b. Competitive rounds — see 06-MULTIPLAYER.md
// ---------------------------------------------------------------------------

export type MatchPhase = 'lobby' | 'countdown' | 'running' | 'grading' | 'results' | 'cancelled';

/** Ranking metric. 'completion' = pure similarity to target, the stated game rule. */
export type RankBy = 'completion' | 'final';

export interface MatchConfig {
  durationMs: number;
  rankBy: RankBy;
  maxPlayers: number;
  /** Omit to have the server pick/generate; all players always receive the identical item. */
  challengeRef?: { challengeId: string; version: number };
  /** Per-player submission rate limit; guards replay capacity. */
  minSubmitIntervalMs: number;
  /**
   * Which editor the round is played in; everyone uses the same one. Absent means `servo`.
   *
   * A submission scored in another mode is refused with `WRONG_PROGRAMMING_MODE`, and creation
   * fails if the challenge does not support it. See 06-MULTIPLAYER.md §3.
   */
  programmingMode?: ProgrammingMode;
}

export interface MatchState {
  matchId: string;
  phase: MatchPhase;
  config: MatchConfig;
  /** Server epoch-ms. Authoritative — clients render a countdown, the server decides acceptance. */
  opensAt: number | null;
  closesAt: number | null;
  serverTime: number;
  players: Array<{ playerId: string; displayName: string; connected: boolean; submitted: boolean }>;
}

/** Returned during `running`. Deliberately carries NO score. */
export interface MatchSubmissionAck {
  submissionId: string;
  accepted: boolean;
  serverReceivedAt: number;
  /** Set when rejected. */
  rejectedReason?: MatchRejection;
}

export type MatchRejection =
  | 'after-deadline' | 'rate-limited' | 'not-participant' | 'wrong-phase'
  | 'wrong-challenge'
  /** Right challenge, wrong editor — the fix is to switch modes, not to find another round. */
  | 'wrong-programming-mode';

export interface MatchResultRow {
  rank: number;
  playerId: string;
  displayName: string;
  completionScore: number;
  finalScore: number;
  metrics: ProgramMetrics;
  submissionId: string | null;
  /** Published so a disputed deadline decision is auditable after the fact. */
  serverReceivedAt: number | null;
}

export interface MatchResults {
  matchId: string;
  challengeId: string;
  challengeVersion: number;
  rankBy: RankBy;
  /** The round is single-mode, so this applies to every row. Absent means `servo`. */
  programmingMode?: ProgrammingMode;
  rows: MatchResultRow[];
}

// ---------------------------------------------------------------------------
// 7. Message kind → payload map
// ---------------------------------------------------------------------------

export interface HcrMessages {
  'catalog.list.req': Record<string, never>;
  'catalog.list.res': ChallengeSummary[];
  'catalog.get.req': { challengeId: string; version?: number };
  'catalog.get.res': ChallengeDefinitionDto;

  'submission.create.req': SubmissionCreate;
  'submission.accepted.res': SubmissionAccepted;
  'submission.result.evt': SubmissionResult;

  'session.start.req': SessionStart;
  'session.start.res': SessionSnapshot;
  'session.next.req': { sessionId: string };
  'session.next.res': NextItem;
  'session.respond.req': SessionRespond;
  'session.respond.res': ResponseOutcome;
  'session.finalize.req': { sessionId: string };
  'session.finalize.res': SessionResultDto;

  'match.create.req': MatchConfig;
  'match.create.res': MatchState;
  'match.join.req': { matchId: string };
  'match.join.res': MatchState;
  'match.leave.req': { matchId: string };
  'match.time.req': { clientSentAt: number };
  'match.time.res': { clientSentAt: number; serverTime: number };
  'match.state.evt': MatchState;
  'match.challenge.evt': ChallengeDefinitionDto;
  'match.results.evt': MatchResults;
  'match.presence.evt': { playerId: string; event: 'join' | 'leave' | 'disconnect' };

  'dev.state.evt': DeviceState;
  'dev.telemetry.evt': DeviceTelemetry;
  'dev.event.evt': DeviceEvent;
  'dev.ack.evt': DeviceAck;
  'dev.cmd.req': DeviceCommand;
  'dev.estop.req': { reason: string };
  'dev.cfg.req': { axes: AxisConfig[] };

  'error': HcrError;
}

export type HcrKind = keyof HcrMessages;
export type HcrEnvelope<K extends HcrKind = HcrKind> = Envelope<K, HcrMessages[K]>;

// ---------------------------------------------------------------------------
// 8. Provider interfaces the backend implementations must satisfy
// ---------------------------------------------------------------------------

/**
 * These are the EXISTING v1 interfaces from src/services/contracts.ts, restated so
 * this file documents the full seam. HTTP/MQTT implementations satisfy them as-is;
 * no UI or engine change is required (SPEC v0.3 §15 compliance).
 *
 *   interface ChallengeProvider {
 *     listChallenges(): Promise<ChallengeSummary[]>;
 *     getChallenge(id: string): Promise<Challenge>;
 *   }
 *   interface ScoreProvider {
 *     score(input: ScoreInput): Promise<ScoreResult>;
 *   }
 *
 * New capabilities arrive as NEW interfaces, never by widening the two above:
 */
export interface AssessmentProvider {
  startSession(input: SessionStart): Promise<SessionSnapshot>;
  nextItem(sessionId: string): Promise<NextItem>;
  submit(input: SubmissionCreate): Promise<SubmissionResult>;
  respond(input: SessionRespond): Promise<ResponseOutcome>;
  finalize(sessionId: string): Promise<SessionResultDto>;
}

export interface DeviceProvider {
  listDevices(): Promise<Array<{ deviceId: string; state: DeviceState }>>;
  run(deviceId: string, program: Program): Promise<DeviceAck>;
  estop(deviceId: string, reason: string): Promise<DeviceAck>;
  subscribeTelemetry(deviceId: string, onSample: (t: DeviceTelemetry) => void): () => void;
}
