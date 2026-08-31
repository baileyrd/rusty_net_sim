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
use source::{RemoteStateSource, RemoteUpdate, ServerOnlySource};
use world::AuthoritativeWorld;

/// `blend_frames` at or below this is treated as an aggressive-enough
/// correction to count as a visible "pop" in the metrics. First-pass
/// threshold, same spirit as the ported reconciler's own first-guess curve.
const POP_BLEND_FRAMES_THRESHOLD: u8 = 4;

/// A single simulated client: dead-reckons its local prediction each tick,
/// and reconciles against whatever authoritative update its `NetworkShim`
/// delivers.
///
/// `blend_frames` is recorded as the reconciler's output for offline
/// tuning (per the design doc's metrics schema) but this harness has no
/// renderer to animate a multi-frame visual blend over — on a correction,
/// `predicted` is snapped straight to the reconciled target. Multi-tick
/// interpolation is a rendering-layer concern for a future real client.
struct SimulatedClient {
    id: String,
    dt: f32,
    predicted: EntityState,
    source: ServerOnlySource,
    reconciler: RollbackReconciler,
    last_residual: f32,
    last_blend_frames: u8,
}

impl SimulatedClient {
    fn new(id: String, dt: f32, initial: EntityState, source: ServerOnlySource) -> Self {
        Self {
            id,
            dt,
            predicted: initial,
            source,
            reconciler: RollbackReconciler::new(),
            last_residual: 0.0,
            // 0 is outside the reconciler's [1, 8] range: it marks "no
            // reconciliation has happened yet" for offline analysis.
            last_blend_frames: 0,
        }
    }

    /// Advances local prediction by dead reckoning, then reconciles against
    /// a freshly delivered authoritative update if one arrived this tick.
    ///
    /// Returns `(residual, blend_frames, pop)` for this tick's metrics
    /// record. `residual`/`blend_frames` carry forward the most recent
    /// reconciliation until a new one occurs; `pop` is true only on the
    /// tick a correction aggressive enough to be visually jarring fires.
    fn step(&mut self, tick: u64) -> (f32, u8, bool) {
        for axis in 0..3 {
            self.predicted.position[axis] += self.predicted.velocity[axis] * self.dt;
        }

        let mut pop = false;
        if let Some(update) = self.source.latest(tick) {
            // `update` is the authoritative snapshot as of `update.tick`,
            // already `tick - update.tick` ticks stale by the time it
            // arrives. Rollback reconciliation means re-extrapolating that
            // historical snapshot forward to *now* along its own velocity
            // before treating it as the correction target — otherwise every
            // correction chases an already-outdated position and "residual"
            // just measures network delay instead of prediction error.
            let elapsed = tick.saturating_sub(update.tick) as f32 * self.dt;
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
            let client_seed = config.seed.wrapping_add(index as u64 + 1);
            let shim: NetworkShim<RemoteUpdate> =
                NetworkShim::new(client_cfg.lag_profile, config.tick_rate_hz, client_seed);
            let source = ServerOnlySource::new(shim);
            SimulatedClient::new(client_cfg.id.clone(), dt, initial_state, source)
        })
        .collect();

    let mut recorder = MetricsRecorder::create(&config.output_path)?;

    for _ in 0..config.tick_count {
        let snapshot = world.step();
        for client in &mut clients {
            client.source.send(
                snapshot.tick,
                RemoteUpdate {
                    tick: snapshot.tick,
                    state: snapshot.state,
                },
            );
        }
        for client in &mut clients {
            let (residual, blend_frames, pop) = client.step(snapshot.tick);
            recorder.record(snapshot.tick, &client.id, residual, blend_frames, pop)?;
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

    #[test]
    fn reconciliation_snaps_predicted_to_the_reconciled_target() {
        let dt = 1.0 / 60.0;
        let shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 1);
        let source = ServerOnlySource::new(shim);
        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client = SimulatedClient::new("c1".to_string(), dt, initial, source);

        let target = EntityState::new([100.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        client.source.send(
            1,
            RemoteUpdate {
                tick: 1,
                state: target,
            },
        );

        let (residual, blend_frames, _pop) = client.step(1);
        assert!(residual > 0.0);
        assert!((1..=8).contains(&blend_frames));
        assert_eq!(client.predicted.position, target.position);
        assert_eq!(client.predicted.velocity, target.velocity);
    }

    #[test]
    fn pop_true_when_confidence_bottoms_out() {
        let dt = 1.0 / 60.0;
        let shim: NetworkShim<RemoteUpdate> = NetworkShim::new(zero_lag_profile(), 60, 2);
        let source = ServerOnlySource::new(shim);
        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client = SimulatedClient::new("c1".to_string(), dt, initial, source);

        // Repeated huge, ever-moving-target residuals drive confidence to
        // zero, so the reconciler eventually settles on blend_frames == 1
        // and the correction counts as a pop.
        let mut last_pop = false;
        let mut far = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        for tick in 1..=50u64 {
            far.position[0] += 10_000.0;
            client.source.send(
                tick,
                RemoteUpdate {
                    tick,
                    state: far,
                },
            );
            let (_, _, pop) = client.step(tick);
            last_pop = pop;
        }
        assert!(last_pop);
        assert_eq!(client.predicted.position, far.position);
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
        let source = ServerOnlySource::new(shim);
        let initial = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let mut client = SimulatedClient::new("c1".to_string(), dt, initial, source);

        let (residual, blend_frames, pop) = client.step(1);
        assert_eq!(residual, 0.0);
        assert_eq!(blend_frames, 0);
        assert!(!pop);
    }
}
