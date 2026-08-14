/**
 * Cutter Grid trajectory fixture generator.
 *
 * Drives the REAL browser planner (`planCutterGridLadderTrajectory`, the same
 * function the app runs in its Web Worker) over the certified reference program
 * and records the plan it produced. The Rust verifier is then asserted against
 * that output by `hcr-backend/crates/hcr_sim/tests/cutter.rs`.
 *
 * Same principle as `generate-vectors.ts`: the TypeScript planner is the
 * incumbent definition of correct, and this file never encodes an expectation of
 * its own. Testing the verifier against a hand-written plan would only prove it
 * agrees with whatever a test author imagined a trajectory looks like — where
 * the reference program has an *independently known* answer, since the profile
 * certifies that this route removes exactly the twelve target voxels and nothing
 * else. A completion score of anything but 100 means the two engines disagree
 * about what the tool touched.
 *
 * Run from the frontend package, which is where the toolchain lives:
 *
 *   cd HCR_Simulator_Frontend && npm run cutter-grid:plan
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';

import { defaultChallengeDefinition } from '../../HCR_Simulator_Frontend/src/data/challenges/defaultChallenge';
import { expandCutterGridProgram } from '../../HCR_Simulator_Frontend/src/features/cutter-grid/programCompiler';
import { planCutterGridLadderTrajectory } from '../../HCR_Simulator_Frontend/src/features/cutter-grid/ladderPlanner';
import { registeredCutterGridProfileV2 } from '../../HCR_Simulator_Frontend/src/features/cutter-grid/profileRegistry';
import { CUTTER_GRID_LADDER_PLANNER_VERSION } from '../../HCR_Simulator_Frontend/src/features/cutter-grid/types';
import type { CutterGridProgramV1 } from '../../HCR_Simulator_Frontend/src/features/cutter-grid/types';
import { normalizeChallenge } from '../../HCR_Simulator_Frontend/src/services/normalizeChallenge';

// Relative to this file, not to `process.cwd()`: the generator is invoked
// through a config in the frontend package, so the working directory is not
// something it can assume.
const OUTPUT = resolve(
  dirname(fileURLToPath(import.meta.url)),
  '../crates/hcr_sim/tests/fixtures/cutter-grid-plan-v2.json',
);

it('generates a certified Cutter Grid trajectory plan', () => {
  const challenge = normalizeChallenge(defaultChallengeDefinition);
  const profile = registeredCutterGridProfileV2(challenge);
  expect(profile, 'the bundled V2 profile must match the shipped challenge').toBeDefined();
  if (!profile) return;

  // The profile stores its reference program tagged at V1's planner version;
  // the ladder planner refuses anything not tagged V2, and rightly so.
  const program: CutterGridProgramV1 = {
    ...profile.referenceProgram,
    plannerVersion: CUTTER_GRID_LADDER_PLANNER_VERSION,
  };
  const runtimeActions = expandCutterGridProgram(program);
  const plan = planCutterGridLadderTrajectory(challenge, {
    program,
    runtimeActions,
    executedCommandCount: runtimeActions.length,
  }, profile);

  expect(plan.version).toBe(2);
  expect(plan.challengeSignature).toBe(profile.challengeSignature);
  expect(plan.executedCommandCount).toBe(runtimeActions.length);

  // Minified: this is a fixture read by a Rust `include_str!`, never by a
  // human, and pretty-printing it costs ~700 kB of pure whitespace.
  mkdirSync(dirname(OUTPUT), { recursive: true });
  writeFileSync(OUTPUT, `${JSON.stringify({ program, plan })}\n`);

  const waypoints =
    plan.positioningTrajectory.length +
    plan.steps.reduce((total, step) => total + step.waypoints.length, 0);
  console.log(
    `wrote ${OUTPUT}: ${plan.steps.length} steps, ${waypoints} waypoints`,
  );
});
