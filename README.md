# rusty_net_sim

A standalone harness for developing and tuning client-side prediction /
rollback reconciliation logic against controlled, reproducible network
conditions — without needing a live match to observe it in. It answers
questions like "how much does 80ms of jitter actually cost us in visible
pop" by running many simulated clients against one authoritative server
under a seeded, deterministic network model.

Single process, single binary crate. No real sockets, no real physics —
just a simplified kinematic entity (position + velocity) moving under
deterministic synthetic motion, and a confidence-weighted reconciler
deciding how aggressively each client should correct toward the truth.

## Quickstart

```sh
cargo run -- scenario.toml
```

This runs the scenario described in `scenario.toml`, and writes one JSON
Lines record per (tick, client) to the configured output path
(`metrics.jsonl` by default).

```sh
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## How it works

Each tick:

1. `AuthoritativeWorld` advances the one shared entity's ground-truth
   position and emits a snapshot.
2. The snapshot is enqueued into every client's `NetworkShim`, which
   schedules delivery `latency ± jitter` ticks later (or drops it per
   `loss_pct`) using a seeded RNG.
3. Each client dead-reckons its local prediction forward every tick, and
   when a delayed snapshot is finally delivered, re-extrapolates it to
   the current tick before treating it as the correction target —
   otherwise "residual" would just measure network delay instead of
   prediction error.
4. `RollbackReconciler` tracks a rolling confidence score per client
   (an EWMA of recent position error) and derives `blend_frames`
   (1–8) from it: low confidence → snap fast, high confidence → smooth.
5. Every (tick, client) gets one metrics record.

Each client also runs a second, independent shadow track fed by
`PeerPublishSource`: clients are wired in a ring (client *i* publishes
its own server-reconciled belief to client *i+1*), and the divergence
between that peer-informed track and the real server-informed one is
recorded as `peer_divergence` — a first experiment in whether trusting a
nearby peer would have helped or hurt versus trusting the server alone.

Fixed tick rate, no wall-clock dependency, one seeded RNG per shim. Same
seed + same scenario config always produces a bit-identical run, which is
the actual point of simulating instead of testing against a live match —
it makes comparisons mean something, since every run being compared saw
identical network conditions.

## Scenario config

```toml
seed = 42
tick_rate_hz = 60
tick_count = 3600        # 60s at 60Hz
output_path = "metrics.jsonl"   # optional, defaults to metrics.jsonl

[peer]                   # optional; defaults to a fast/LAN-like link
latency_ms = 5
jitter_ms = 1
loss_pct = 0.0

[[clients]]
id = "c1"
latency_ms = 20
jitter_ms = 5
loss_pct = 0.0

# ... more [[clients]] entries
```

See `scenario.toml` for a full 8-client example with mixed lag profiles.

## Metrics output

JSON Lines, one record per (tick, client):

```json
{"tick": 412, "client": "c2", "residual": 34.2, "blend_frames": 3, "pop": false, "peer_divergence": 12.7}
```

| Field | Meaning |
|---|---|
| `tick` | Simulation tick this record was written at |
| `client` | Client id |
| `residual` | Position error at the most recent server reconciliation (carried forward between reconciliations) |
| `blend_frames` | Reconciler's correction aggressiveness, 1 (snap) – 8 (smooth); `0` means this client hasn't reconciled yet |
| `pop` | True on the tick a correction fired aggressively enough (`blend_frames <= 4`) to count as a visible pop |
| `peer_divergence` | Distance between the peer-informed and server-informed reconciled positions; `0.0` until both tracks have reconciled at least once |

Deliberately plain JSONL rather than a bundled plotting dependency —
analysis happens separately in whatever's convenient (a notebook, a
script, a spreadsheet).

## Architecture

| Module | Responsibility |
|---|---|
| `entity.rs` | `EntityState` (position, velocity) and `position_error()` |
| `world.rs` | `AuthoritativeWorld` — ground-truth entity, seeded synthetic waypoint-steering motion |
| `shim.rs` | `NetworkShim` — per-connection latency/jitter/loss, seeded delivery queue |
| `reconciler.rs` | `ConfidenceTracker` / `ReconciliationPolicy` / `RollbackReconciler` |
| `source.rs` | `RemoteStateSource` trait; `ServerOnlySource` and `PeerPublishSource` |
| `metrics.rs` | `MetricsRecorder` — JSONL writer |
| `config.rs` | `ScenarioConfig` — TOML scenario definition |
| `main.rs` | Wires it all together: world, per-client server + peer tracks, ring topology, metrics loop |

## Scope

**In scope:** one authoritative server, up to 8 simulated clients, a
single shared kinematic entity, per-client network shims, baseline
(`ServerOnlySource`) and peer-comparison (`PeerPublishSource`)
reconciliation modes, JSONL metrics.

**Explicitly out of scope for this repo:** real physics/collision (a
later integration task against this same shim/reconciler boundary), real
sockets, cryptographic signing / peer-trust verification, dynamic
client join/leave, more than 8 clients, multiple entities per client,
replay-driven motion (synthetic scripted motion is the only motion
source for now).
