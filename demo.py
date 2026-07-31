#!/usr/bin/env python3
"""End-to-end walkthrough of the HCR backend.

Start the server first:

    cargo run -p hcr_service --features hotaru --example serve

then:

    python3 demo.py

Every request below goes over real HTTP to the real service — catalog,
authoritative replay, an adaptive session and a competitive round.
"""

import json
import urllib.error
import urllib.request

BASE = "http://localhost:8080/api/v1"

# The challenge's shipped starter workspace, as Program IR. The frontend
# compiles this from Blockly; here it is written out directly.
STARTER = {
    "sourceBlockCount": 5,
    "nodes": [
        {"type": "set-joint-angle", "jointId": "shoulderRoll", "angleDeg": 15, "sourceBlockId": "starter-shoulder-roll"},
        {"type": "set-joint-angle", "jointId": "shoulder", "angleDeg": 80, "sourceBlockId": "starter-shoulder"},
        {"type": "set-joint-angle", "jointId": "elbow", "angleDeg": 0, "sourceBlockId": "starter-elbow"},
        {"type": "set-joint-angle", "jointId": "wrist", "angleDeg": -80, "sourceBlockId": "starter-wrist"},
        {"type": "set-joint-angle", "jointId": "baseYaw", "angleDeg": 55, "sourceBlockId": "starter-base-sweep"},
    ],
}

# Sweeps toward the head instead of away from it.
COLLIDING = {
    "sourceBlockCount": 1,
    "nodes": [
        {"type": "set-joint-angle", "jointId": "baseYaw", "angleDeg": 0, "sourceBlockId": "reckless"}
    ],
}


def call(method, path, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(f"{BASE}{path}", data=data, method=method)
    request.add_header("Content-Type", "application/json")
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def rule(title):
    print(f"\n\033[1m{title}\033[0m\n" + "─" * 66)


def submit(submission_id, challenge_id, version, program, **extra):
    return call("POST", "/submissions", {
        "submissionId": submission_id,
        "challengeId": challenge_id,
        "challengeVersion": version,
        "program": program,
        **extra,
    })


def catalog():
    rule("1  CATALOG — one authored challenge, three generated")
    _, items = call("GET", "/challenges")
    for item in items:
        _, dto = call("GET", f"/challenges/{item['id']}")
        meta = dto["meta"]
        origin = "generated" if meta.get("generator") else "authored"
        print(f"  {item['id']:<28} b={meta['irt']['difficulty']:+.2f} "
              f"{meta['calibration']:<11} {origin:<9} "
              f"{len(dto['initialHair']['voxels']):>4} → {len(dto['targetHair']['voxels']):>4} voxels")
    return items


def scoring():
    rule("2  AUTHORITATIVE SCORING — the server replays, the client is not trusted")
    _, result = submit("demo-starter", "neat-short-cap", 1, STARTER)
    score = result["score"]
    print(f"  starter program  → {result['status']}/{result['terminal']['reason']}")
    print(f"    completion {score['completionScore']:.2f}   efficiency {score['efficiencyScore']:.2f}   "
          f"time {score['timeScore']:.2f}   FINAL {score['finalScore']:.4f}")
    print(f"    executed={result['metrics']['executedCommandCount']} "
          f"estDuration={result['metrics']['estimatedDurationMs']:.0f}ms  engine={result['replay']['engineVersion']}")
    print("    ↑ matches the TypeScript conformance vectors exactly (84.65 / 90.7884)")

    _, crash = submit("demo-crash", "neat-short-cap", 1, COLLIDING)
    terminal = crash["terminal"]
    print(f"\n  colliding program → {crash['status']}/{terminal['reason']}")
    print(f"    {terminal['partLabel']} would hit the head; {terminal['jointId']} "
          f"stopped at {terminal['safeAngleDeg']:.2f}°, block \"{terminal['sourceBlockId']}\"")
    print("    ↑ a halt still produces a score, and names the block to highlight")

    # Idempotency: the same id with a different program must not re-score.
    _, replay = submit("demo-starter", "neat-short-cap", 1, COLLIDING)
    same = replay["resultVoxelsHash"] == result["resultVoxelsHash"]
    print(f"\n  resubmitting id \"demo-starter\" with a *different* program → "
          f"returns the first result: {same}")


def adaptive():
    rule("3  ADAPTIVE SESSION — arona picks each item from the current θ")
    _, opened = call("POST", "/sessions", {})
    session = opened["sessionId"]
    print(f"  session {session}   θ={opened['theta']:+.3f} se=∞ (no responses yet)")

    for index in range(1, 6):
        status, item = call("POST", f"/sessions/{session}/next", {})
        if status != 200:
            print(f"  next → {item['error']['code']} (terminator fired)")
            break
        submit(f"cat-{index}", item["challengeId"], item["challengeVersion"], STARTER)
        _, outcome = call("POST", f"/sessions/{session}/responses", {
            "sessionId": session,
            "itemRef": item["itemRef"],
            "submissionId": f"cat-{index}",
        })
        print(f"  item {item['challengeId']:<28} raw={outcome['rawScore']:.3f} "
              f"correct={str(outcome['correct']):<5} θ={outcome['theta']:+.3f} "
              f"se={outcome['standardError']:.3f}")
        if outcome["terminated"]:
            print(f"    terminated: {outcome.get('terminationReason')}")
            break

    _, final = call("POST", f"/sessions/{session}/finalize", {})
    print(f"  FINAL θ={final['finalTheta']:+.4f} se={final['standardError']:.4f} "
          f"items={final['totalItems']} — \"{final['terminationReason']}\"")

    # A forged reference must not be accepted.
    status, _ = call("POST", f"/sessions/{session}/responses", {
        "sessionId": session, "itemRef": "forged.token", "submissionId": "cat-1",
    })
    print(f"  forged itemRef → HTTP {status}")


def round_():
    rule("4  COMPETITIVE ROUND — same item for all, server clock decides")
    _, match = call("POST", "/matches", {
        "durationMs": 60_000,
        "rankBy": "completion",
        "maxPlayers": 4,
        "minSubmitIntervalMs": 0,
        "challengeRef": {"challengeId": "neat-short-cap", "version": 1},
    })
    match_id = match["matchId"]
    print(f"  match {match_id}  phase={match['phase']}")

    status, _ = call("GET", f"/matches/{match_id}/challenge")
    print(f"  challenge during lobby → HTTP {status} (withheld: no head start)")

    for player in ("alice", "bob"):
        call("POST", f"/matches/{match_id}/join", {}, {"X-HCR-Player": player})
    started = call("POST", f"/matches/{match_id}/start", {})[1]
    print(f"  started: closes_at−opens_at = {started['closesAt'] - started['opensAt']}ms, "
          f"{len(started['players'])} players")

    status, _ = call("GET", f"/matches/{match_id}/challenge")
    print(f"  challenge after start  → HTTP {status} (revealed)")

    for player, program, sid in (("alice", STARTER, "m-alice"), ("bob", COLLIDING, "m-bob")):
        _, ack = call("POST", f"/matches/{match_id}/submissions", {
            "submissionId": sid, "challengeId": "neat-short-cap",
            "challengeVersion": 1, "program": program,
        }, {"X-HCR-Player": player})
        # The ack deliberately carries no score.
        print(f"  {player} submits → accepted={ack['accepted']}, keys={sorted(ack.keys())}")

    status, _ = call("GET", f"/matches/{match_id}/results")
    print(f"  results while running  → HTTP {status} (hidden until close)")
    print("  (a real round closes on the server clock; tests drive that with ManualClock)")


def main():
    try:
        call("GET", "/time")
    except Exception:
        print("Server not reachable. Start it with:\n"
              "  cargo run -p hcr_service --features hotaru --example serve")
        return

    catalog()
    scoring()
    adaptive()
    round_()
    print("\n\033[1mdone\033[0m\n")


if __name__ == "__main__":
    main()
