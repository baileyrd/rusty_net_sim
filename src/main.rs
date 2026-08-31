mod config;
mod entity;
mod metrics;
mod reconciler;
mod shim;
mod source;
mod world;

use config::ScenarioConfig;
use entity::EntityState;
use metrics::MetricsRecorder;
use reconciler::RollbackReconciler;
use shim::NetworkShim;
use source::{PeerPublishSource, RemoteStateSource, RemoteUpdate, ServerOnlySource};
use world::AuthoritativeWorld;

/// `blend_frames` at or below this is treated as an aggressive-enough
/// correction to count as a visible "pop" in the metrics. First-pass
/// threshold, same spirit as the ported reconciler's own first-guess curve.
const POP_BLEND_FRAMES_THRESHOLD: u8 = 4;

/// One dead-reckoning + reconciliation track: a predicted `EntityState`
/// advanced each tick and periodically corrected against whatever a
/// `RemoteStateSource` delivers. Shared by both the canonical (server)
/// track every client has always had, and the phase 2 shadow (peer) track
/// used only to measure how the two sources would have diverged.
///
/// `blend_frames` is recorded as the reconciler's output for offline
/// tuning (per the design doc's metrics schema) but this harness has no
/// renderer to animate a multi-frame visual blend over — on a correction,
/// `predicted` is snapped straight to the reconciled target. Multi-tick
/// interpolation is a rendering-layer concern for a future real client.
struct ReconciledTrack {
    predicted: EntityState,
    reconciler: RollbackReconciler,
    last_residual: f32,
    last_blend_frames: u8,
}

impl ReconciledTrack {
    fn new(initial: EntityState) -> Self {
        Self {
            predicted: initial,
            reconciler: RollbackReconciler::new(),
            last_residual: 0.0,
            // 0 is outside the reconciler's [1, 8] range: it marks "no
            // reconciliation has happened yet" for offline analysis.
            last_blend_frames: 0,
        }
    }

    /// Advances local prediction by dead reckoning, then reconciles against
    /// a freshly delivered update from `source` if one arrived this tick.
    ///
    /// Returns `(residual, blend_frames, pop)` for this tick.
    /// `residual`/`blend_frames` carry forward the most recent
    /// reconciliation until a new one occurs; `pop` is true only on the
    /// tick a correction aggressive enough to be visually jarring fires.
    fn step(&mut self, source: &mut impl RemoteStateSource, tick: u64, dt: f32) -> (f32, u8, bool) {
        for axis in 0..3 {
            self.predicted.position[axis] += self.predicted.velocity[axis] * dt;
        }

        let mut pop = false;
        if let Some(update) = source.latest(tick) {
            // `update` is the source's snapshot as of `update.tick`, already
            // `tick - update.tick` ticks stale by the time it arrives.
            // Rollback reconciliation means re-extrapolating that historical
            // snapshot forward to *now* along its own velocity before
            // treating it as the correction target — otherwise every
            // correction chases an already-outdated position and "residual"
            // just measures network delay instead of prediction error.
            let elapsed = tick.saturating_sub(update.tick) as f32 * dt;
            let mut target = update.state;
            for axis in 0..3 {
                target.position[axis] += target.velocity[axis] * elapsed;
            }

            let residual = self.predicted.position_error(&target);
            let policy = self.reconciler.reconcile(self.predicted, target);

            self.predicted = target;

            self.last_residual = residual;
            self.last_blend_frames = policy.blend_frames;
            pop = policy.blend_frames <= POP_BLEND_FRAMES_THRESHOLD;
        }

        (self.last_residual, self.last_blend_frames, pop)
    }
}

/// A single simulated client. Keeps two independent reconciliation tracks:
///
/// - `server_track`, fed by `server_source` (`ServerOnlySource`) — the
///   client's real, canonical predicted state, unchanged from phase 1.
/// - `peer_track`, fed by `peer_source` (`PeerPublishSource`) — a shadow
///   track that never affects `server_track`, reconciled against whatever
///   this client's ring neighbor publishes. Its only purpose is measuring
///   `peer_divergence`: how far a peer-informed reconciliation would have
///   drifted from the server-informed one, to answer "would trusting a
///   peer here have helped or hurt."
struct SimulatedClient {
    id: String,
    dt: f32,
    server_source: ServerOnlySource,
    server_track: ReconciledTrack,
    peer_source: PeerPublishSource,
    peer_track: ReconciledTrack,
    last_peer_divergence: f32,
}

impl SimulatedClient {
    fn new(
        id: String,
        dt: f32,
        initial: EntityState,
        server_source: ServerOnlySource,
        peer_source: PeerPublishSource,
    ) -> Self {
        Self {
            id,
            dt,
            server_source,
            server_track: ReconciledTrack::new(initial),
            peer_source,
            peer_track: ReconciledTrack::new(initial),
            last_peer_divergence: 0.0,
        }
    }

    /// Steps the canonical server-informed track. Returns
    /// `(residual, blend_frames, pop)` for the metrics record.
    fn step_server(&mut self, tick: u64) -> (f32, u8, bool) {
        self.server_track.step(&mut self.server_source, tick, self.dt)
    }

    /// The state this client publishes to its ring neighbor: its own
    /// canonical (server-informed) belief about the shared entity.
    fn publish_state(&self) -> EntityState {
        self.server_track.predicted
    }

    /// Steps the shadow peer-informed track and updates `peer_divergence`
    /// against the (already-stepped) server track. Divergence stays at its
    /// prior value until both tracks have reconciled at least once.
    fn step_peer(&mut self, tick: u64) -> f32 {
        self.peer_track.step(&mut self.peer_source, tick, self.dt);
        if self.peer_track.last_blend_frames > 0 && self.server_track.last_blend_frames > 0 {
            self.last_peer_divergence = self
                .peer_track
                .predicted
                .position_error(&self.server_track.predicted);
        }
        self.last_peer_divergence
    }
}

fn run(scenario_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = ScenarioConfig::load(scenario_path)?;
    let dt = 1.0 / config.tick_rate_hz as f32;

    let mut world = AuthoritativeWorld::new(config.seed, config.tick_rate_hz);
    let initial_state = world.state();

    let mut clients: Vec<SimulatedClient> = config
        .clients
        .iter()
        .enumerate()
        .map(|(index, client_cfg)| {
            // Distinct per-client seed derived from the scenario seed, so
            // the whole run stays reproducible from one top-level seed.
            let server_seed = config.seed.wrapping_add(index as u64 + 1);
            let server_shim: NetworkShim<RemoteUpdate> =
                NetworkShim::new(client_cfg.lag_profile, config.tick_rate_hz, server_seed);
            let server_source = ServerOnlySource::new(server_shim);

            // A distinct seed range for peer links, so they don't share RNG
            // state with any server link even for identical LagProfiles.
            let peer_seed = config.seed.wrapping_add(1_000_000 + index as u64);
            let peer_shim: NetworkShim<RemoteUpdate> =
                NetworkShim::new(config.peer, config.tick_rate_hz, peer_seed);
            let peer_source = PeerPublishSource::new(peer_shim);

            SimulatedClient::new(client_cfg.id.clone(), dt, initial_state, server_source, peer_source)
        })
        .collect();

    let mut recorder = MetricsRecorder::create(&config.output_path)?;
    let client_count = clients.len();

    for _ in 0..config.tick_count {
        let snapshot = world.step();

        // 1. Server broadcasts ground truth to every client.
        for client in &mut clients {
            client.server_source.send(
                snapshot.tick,
                RemoteUpdate {
                    tick: snapshot.tick,
                    state: snapshot.state,
                },
            );
        }

        // 2. Each client dead-reckons + reconciles its canonical track.
        let mut server_records = Vec::with_capacity(client_count);
        for client in &mut clients {
            server_records.push(client.step_server(snapshot.tick));
        }

        // 3. Ring: each client publishes its just-updated canonical belief
        //    to its one neighbor (client i -> client (i + 1) % N).
        if client_count > 0 {
            let published: Vec<EntityState> =
                clients.iter().map(SimulatedClient::publish_state).collect();
            for (i, state) in published.into_iter().enumerate() {
                let next = (i + 1) % client_count;
                clients[next].peer_source.publish(
                    snapshot.tick,
                    RemoteUpdate {
                        tick: snapshot.tick,
                        state,
                    },
                );
            }
        }

        // 4. Each client steps its shadow peer track and records metrics.
        for (client, (residual, blend_frames, pop)) in clients.iter_mut().zip(server_records) {
            let peer_divergence = client.step_peer(snapshot.tick);
            recorder.record(
                snapshot.tick,
                &client.id,
                residual,
                blend_frames,
                pop,
                peer_divergence,
            )?;
        }
    }

    recorder.flush()?;
    Ok(())
}

fn main() {
    let scenario_path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: rusty_net_sim <scenario.toml>");
            std::process::exit(1);
        }
    };

    if let Err(err) = run(&scenario_path) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shim::LagProfile;

    fn zero_lag_profile() -> LagProfile {
        LagProfile {
            latency_ms: 0,
            jitter_ms: 0,
            loss_pct: 0.0,
        }
    }

    /// A peer link so slow it never delivers within a short test — used
    /// when a test only cares about the server track.
    fn inert_peer_source(seed: u64) -> PeerPublishSource {
        let profile = LagProfile {
            latency_ms: 1_000_000,
            jitter_ms: 0,
            loss_pct: 0.0,
        };
        PeerPublishSource::new(NetworkShim::new(profile, 60, seed))
    }

    #[test]
    fn reconciliation_snaps_predicted_to_the_reconciled_target() {
        let dt = 1.0 / 60.0;
        let shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 1);
        let server_source = ServerOnlySource::new(shim);
        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client = SimulatedClient::new(
            "c1".to_string(),
            dt,
            initial,
            server_source,
            inert_peer_source(100),
        );

        let target = EntityState::new([100.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        client.server_source.send(
            1,
            RemoteUpdate {
                tick: 1,
                state: target,
            },
        );

        let (residual, blend_frames, _pop) = client.step_server(1);
        assert!(residual > 0.0);
        assert!((1..=8).contains(&blend_frames));
        assert_eq!(client.server_track.predicted.position, target.position);
        assert_eq!(client.server_track.predicted.velocity, target.velocity);
    }

    #[test]
    fn pop_true_when_confidence_bottoms_out() {
        let dt = 1.0 / 60.0;
        let shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 2);
        let server_source = ServerOnlySource::new(shim);
        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client = SimulatedClient::new(
            "c1".to_string(),
            dt,
            initial,
            server_source,
            inert_peer_source(101),
        );

        // Repeated huge, ever-moving-target residuals drive confidence to
        // zero, so the reconciler eventually settles on blend_frames == 1
        // and the correction counts as a pop.
        let mut last_pop = false;
        let mut far = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        for tick in 1..=50u64 {
            far.position[0] += 10_000.0;
            client.server_source.send(
                tick,
                RemoteUpdate {
                    tick,
                    state: far,
                },
            );
            let (_, _, pop) = client.step_server(tick);
            last_pop = pop;
        }
        assert!(last_pop);
        assert_eq!(client.server_track.predicted.position, far.position);
    }

    #[test]
    fn no_delivery_yet_reports_sentinel_blend_frames() {
        let dt = 1.0 / 60.0;
        let profile = LagProfile {
            latency_ms: 1000,
            jitter_ms: 0,
            loss_pct: 0.0,
        };
        let shim: NetworkShim<RemoteUpdate> = NetworkShim::new(profile, 60, 3);
        let server_source = ServerOnlySource::new(shim);
        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client = SimulatedClient::new(
            "c1".to_string(),
            dt,
            initial,
            server_source,
            inert_peer_source(102),
        );

        let (residual, blend_frames, pop) = client.step_server(1);
        assert_eq!(residual, 0.0);
        assert_eq!(blend_frames, 0);
        assert!(!pop);
    }

    #[test]
    fn peer_divergence_stays_at_sentinel_until_both_tracks_have_reconciled() {
        let dt = 1.0 / 60.0;
        // Server never delivers; peer does.
        let server_profile = LagProfile {
            latency_ms: 1_000_000,
            jitter_ms: 0,
            loss_pct: 0.0,
        };
        let server_shim: NetworkShim<RemoteUpdate> = NetworkShim::new(server_profile, 60, 4);
        let server_source = ServerOnlySource::new(server_shim);
        let peer_shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 5);
        let peer_source = PeerPublishSource::new(peer_shim);

        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client =
            SimulatedClient::new("c1".to_string(), dt, initial, server_source, peer_source);

        client.peer_source.publish(
            1,
            RemoteUpdate {
                tick: 1,
                state: EntityState::new([50.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            },
        );
        client.step_server(1);
        let divergence = client.step_peer(1);

        // The peer track reconciled but the server track never has, so
        // divergence stays at its 0.0 sentinel rather than reporting a
        // misleading comparison against a never-corrected server track.
        assert_eq!(divergence, 0.0);
    }

    #[test]
    fn peer_divergence_reports_gap_once_both_tracks_have_reconciled() {
        let dt = 1.0 / 60.0;
        let server_shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 6);
        let server_source = ServerOnlySource::new(server_shim);
        let peer_shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 7);
        let peer_source = PeerPublishSource::new(peer_shim);

        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client =
            SimulatedClient::new("c1".to_string(), dt, initial, server_source, peer_source);

        client.server_source.send(
            1,
            RemoteUpdate {
                tick: 1,
                state: EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            },
        );
        client.peer_source.publish(
            1,
            RemoteUpdate {
                tick: 1,
                state: EntityState::new([30.0, 40.0, 0.0], [0.0, 0.0, 0.0]),
            },
        );

        client.step_server(1);
        let divergence = client.step_peer(1);

        assert_eq!(divergence, 50.0);
    }
}
