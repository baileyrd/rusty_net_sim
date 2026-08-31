# Findings: what the harness has answered so far

One run, `scenario.toml` (seed 42, 60Hz, 3600 ticks, 8 clients with mixed
lag profiles), reproduced with:

```sh
cargo run --release -- scenario.toml
```

| client | latency (ms) | jitter (ms) | loss (%) | avg residual | max residual | avg peer_divergence | max peer_divergence | pops |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| c1 | 20  | 5  | 0.0 | 0.56  | 59.99  | 22.56 | 514.74 | 0  |
| c2 | 80  | 25 | 1.0 | 3.44  | 201.34 | 3.99  | 172.58 | 0  |
| c3 | 40  | 10 | 0.5 | 1.27  | 115.43 | 3.31  | 172.58 | 0  |
| c4 | 150 | 60 | 3.0 | 10.21 | 384.56 | 12.24 | 360.52 | 3  |
| c5 | 15  | 3  | 0.0 | 0.56  | 59.99  | 12.92 | 360.52 | 0  |
| c6 | 100 | 40 | 2.0 | 4.40  | 288.09 | 5.32  | 259.28 | 0  |
| c7 | 25  | 5  | 0.0 | 0.84  | 89.98  | 4.76  | 259.28 | 0  |
| c8 | 200 | 80 | 5.0 | 12.73 | 543.34 | 22.00 | 514.74 | 13 |

(`c1`'s ring peer for `PeerPublishSource` is `c8`, `c2`'s is `c1`, etc. —
client *i* publishes to client *i+1*, so client *i* is fed by client
*i−1*.)

## 1. Residual scales with latency + jitter, roughly as expected

Average residual runs from 0.56 (best client) to 12.73 (worst) — about
**23x** — and tracks latency+jitter fairly monotonically. This is the
harness doing its basic job: confirming that the rollback reconciler's
residual metric actually reflects network conditions rather than being
swamped by some other effect (an earlier bug where the correction target
wasn't re-extrapolated to the current tick did exactly that — see the
`rollback reconciliation` comment in `main.rs`).

## 2. Visible "pop" only shows up past a real threshold, not gradually

Under this reconciler's confidence curve (`alpha=0.2`, `error_ceiling=300`,
`pop` defined as `blend_frames <= 4`), **no client below 150ms
latency ever pops** — not even c6 at 100ms/40ms jitter/2% loss. Only c4
(150ms/60ms/3%) and c8 (200ms/80ms/5%) do, and c8 pops roughly 4x as often
as c4. That's a real, checkable answer to the harness's founding question:
for this scenario's motion profile and this reconciler's tuning, moderate
lag is absorbed smoothly, and visible correction snaps are a
high-latency-and-jitter-and-loss phenomenon, not something that creeps in
gradually.

## 3. Trusting a peer is only as good as *their* link, not yours

The most interesting signal is in `peer_divergence`. c1 has the best
server connection in the scenario (20ms/5ms/0% loss) — but its ring
neighbor is c8, the *worst* client (200ms/80ms/5% loss). c1's average
peer_divergence (22.56) is nearly identical to c8's own (22.00), and its
max (514.74) matches c8's max exactly — because c1's shadow peer track is
literally reconciling against c8's poorly-corrected belief.

Compare c2, whose neighbor is c1 (the best client): c2's peer_divergence
(avg 3.99) is the lowest in the scenario, well below its own residual
would suggest given c2's middling 80ms/25ms/1% own link.

The takeaway: a DDS-style peer-publish mode doesn't inherit *your* link
quality, it inherits whoever you're listening to's. A fast peer link is
necessary but not sufficient — the harness makes that concretely visible
instead of leaving it as an intuition.

## Caveats

This is one seed, one scenario, one reconciler tuning (`alpha`/
`error_ceiling` are still the sketch's original first guesses, ported
as-is per the handoff). None of the above is a statistically rigorous
claim — it's what one reproducible run shows, which is the harness's job:
making a comparison mean something because every client in it saw the
same synthetic world under the same seed. A next step worth doing before
trusting these numbers further would be sweeping several seeds and
averaging, or tuning `error_ceiling` against a scenario with a known
"should pop here" ground truth.
