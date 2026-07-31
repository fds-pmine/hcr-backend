/**
 * Conformance vector generator.
 *
 * Drives the REAL TypeScript engine (`SimulationEngine`, the same class the app
 * runs) over a set of programs and records what it produced. The Rust port is
 * then asserted against this output by
 * `hcr-backend/crates/hcr_sim/tests/conformance.rs`.
 *
 * The TypeScript engine is the incumbent definition of correct; this file never
 * encodes an expectation of its own.
 *
 * Run:
 *   npx vitest run --config hcr-backend/tools/vectors.config.ts
 */
import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { it } from 'vitest';

import { defaultChallengeDefinition } from '../../src/data/challenges/defaultChallenge';
import { expandProgram } from '../../src/features/blockly/programCompiler';
import type {
  Program,
  ProgramNode,
} from '../../src/features/blockly/programTypes';
import { SimulationEngine } from '../../src/features/simulation/SimulationEngine';
import { LocalChallengeProvider } from '../../src/services/local/LocalChallengeProvider';
import { LocalScoreProvider } from '../../src/services/local/LocalScoreProvider';

/** Must equal `hcr_contract::SIM_TICK_MS` so both engines advance identically. */
const TICK_MS = 5;

/** Guard against a program that never terminates. */
const MAX_TICKS = 2_000_000;

const OUTPUT = resolve(
  process.cwd(),
  'hcr-backend/crates/hcr_sim/tests/fixtures/vectors.json',
);

function setAngle(jointId: string, angleDeg: number, id: string): ProgramNode {
  return { type: 'set-joint-angle', jointId, angleDeg, sourceBlockId: id };
}

function wait(durationMs: number, id: string): ProgramNode {
  return { type: 'wait', durationMs, sourceBlockId: id };
}

function repeat(count: number, body: ProgramNode[], id: string): ProgramNode {
  return { type: 'repeat', count, body, sourceBlockId: id };
}

function program(nodes: ProgramNode[], sourceBlockCount: number): Program {
  return { nodes, sourceBlockCount };
}

interface Case {
  id: string;
  note: string;
  program: Program;
}

/**
 * Cases are chosen to cover the branches called out in
 * `docs/backend/02-DETERMINISM.md` §9. Outcomes are NOT predicted here — several
 * of these collide with the head, and recording that faithfully is the point.
 */
const CASES: Case[] = [
  {
    id: 'safe-single-joint',
    note: 'baseYaw -45 -> -60, entirely inside the head-safe band',
    program: program([setAngle('baseYaw', -60, 'a')], 1),
  },
  {
    id: 'wait-only',
    note: 'no motion at all; pins the wait accounting',
    program: program([wait(250, 'a'), wait(250, 'b')], 2),
  },
  {
    id: 'zero-duration-move',
    note: 'target equals the current angle; pins the durationMs === 0 branch',
    program: program([setAngle('baseYaw', -45, 'a')], 1),
  },
  {
    id: 'head-collision',
    note: 'baseYaw sweeps toward 0 and drives the elbow into the head',
    program: program([setAngle('baseYaw', 0, 'a')], 1),
  },
  {
    id: 'repeat-expansion',
    note: 'nested repeat; pins expansion order and executed-command counting',
    program: program(
      [
        repeat(
          3,
          [setAngle('baseYaw', -55, 'a'), setAngle('baseYaw', -40, 'b')],
          'r',
        ),
      ],
      3,
    ),
  },
  {
    id: 'multi-axis',
    note: 'all five joints move; exercises the full kinematic chain',
    program: program(
      [
        setAngle('baseYaw', -55, 'a'),
        setAngle('shoulder', 20, 'b'),
        setAngle('elbow', -120, 'c'),
        setAngle('wrist', -40, 'd'),
        setAngle('shoulderRoll', 20, 'e'),
      ],
      5,
    ),
  },
  {
    id: 'starter-program',
    note: "the challenge's shipped starter workspace — raises the arm to the hair and sweeps",
    program: program(
      [
        setAngle('shoulderRoll', 15, 'starter-shoulder-roll'),
        setAngle('shoulder', 80, 'starter-shoulder'),
        setAngle('elbow', 0, 'starter-elbow'),
        setAngle('wrist', -80, 'starter-wrist'),
        setAngle('baseYaw', 55, 'starter-base-sweep'),
      ],
      5,
    ),
  },
  {
    id: 'starter-then-repeat-sweep',
    note: 'starter pose, then repeated sweeps; removes more hair and raises program cost',
    program: program(
      [
        setAngle('shoulderRoll', 15, 'a'),
        setAngle('shoulder', 80, 'b'),
        setAngle('elbow', 0, 'c'),
        setAngle('wrist', -80, 'd'),
        repeat(
          3,
          [setAngle('baseYaw', 55, 'e'), setAngle('baseYaw', -55, 'f')],
          'r',
        ),
      ],
      7,
    ),
  },
  {
    id: 'high-cost-program',
    note: 'many cheap commands so efficiency and time do NOT clamp to 100',
    program: program(
      [
        repeat(
          20,
          [setAngle('baseYaw', -60, 'a'), setAngle('baseYaw', -35, 'b')],
          'r',
        ),
      ],
      3,
    ),
  },
];

function sha256Hex(input: string): string {
  return createHash('sha256').update(input, 'utf8').digest('hex');
}

it('generates conformance vectors from the TypeScript engine', async () => {
  const challenge = await new LocalChallengeProvider().getChallenge(
    defaultChallengeDefinition.id,
  );
  const scoreProvider = new LocalScoreProvider();

  const cases = [];

  for (const testCase of CASES) {
    // A fresh engine per case: the constructor rebuilds all state from the
    // challenge, so cases cannot leak into one another.
    const engine = new SimulationEngine(challenge, scoreProvider);
    const runtimeCommands = expandProgram(testCase.program);

    engine.run({
      program: testCase.program,
      runtimeCommands,
      executedCommandCount: runtimeCommands.length,
    });

    let ticks = 0;
    while (engine.getSnapshot().status === 'running' && ticks < MAX_TICKS) {
      engine.tick(TICK_MS);
      ticks += 1;
    }
    await engine.waitForScore();

    const snapshot = engine.getSnapshot();
    const remainingVoxels = [...snapshot.hairVoxels].sort();

    cases.push({
      id: testCase.id,
      note: testCase.note,
      program: testCase.program,
      expect: {
        status: snapshot.status,
        errorMessage: snapshot.errorMessage ?? null,
        metrics: snapshot.metrics,
        score: snapshot.scoreResult ?? null,
        jointAngles: snapshot.jointAngles,
        remainingVoxelCount: remainingVoxels.length,
        resultVoxelsHash: sha256Hex(remainingVoxels.join('\n')),
        remainingVoxels,
      },
    });

    // eslint-disable-next-line no-console
    console.log(
      `${testCase.id}: status=${snapshot.status} ` +
        `executed=${snapshot.metrics.executedCommandCount} ` +
        `voxels=${remainingVoxels.length} ` +
        `final=${snapshot.scoreResult?.finalScore.toFixed(4) ?? 'n/a'}`,
    );
  }

  const payload = {
    generator: 'hcr-backend/tools/generate-vectors.ts',
    engine: 'typescript',
    tickMs: TICK_MS,
    initialVoxelCount: challenge.initialHair.voxels.size,
    targetVoxelCount: challenge.targetHair.voxels.size,
    challenge: defaultChallengeDefinition,
    cases,
  };

  mkdirSync(dirname(OUTPUT), { recursive: true });
  writeFileSync(OUTPUT, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');

  // eslint-disable-next-line no-console
  console.log(`\nwrote ${cases.length} vectors -> ${OUTPUT}`);
});
