//! Ground-truth world simulation: advances a single kinematic entity using
//! deterministic, seeded synthetic motion (random waypoint steering).
//!
//! v1 motion source is synthetic scripted movement rather than replay data —
//! the design doc's "open decision," confirmed as the v1 default.

use crate::entity::EntityState;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Per-axis speed bound (world units/sec) for randomly sampled waypoint
/// velocities. Not configurable in v1 — the scenario config only exposes
/// seed/tick-rate/tick-count and per-client network profiles.
const MAX_SPEED: f32 = 800.0;

/// How many seconds between re-sampling a new waypoint velocity target.
const WAYPOINT_INTERVAL_SECS: u64 = 1;

/// A ground-truth snapshot emitted after stepping the world one tick.
#[derive(Clone, Copy, Debug)]
pub struct WorldSnapshot {
    pub tick: u64,
    pub state: EntityState,
}

/// Owns ground-truth entity state and steps it forward each tick using
/// deterministic synthetic motion: a new random velocity is sampled every
/// `WAYPOINT_INTERVAL_SECS` seconds and held constant until the next
/// resample, giving a piecewise-linear, fully reproducible trajectory.
pub struct AuthoritativeWorld {
    tick: u64,
    dt: f32,
    waypoint_interval_ticks: u64,
    next_waypoint_tick: u64,
    state: EntityState,
    rng: StdRng,
}

impl AuthoritativeWorld {
    /// `tick_rate_hz` determines both `dt` and how often the world resamples
    /// its waypoint velocity.
    pub fn new(seed: u64, tick_rate_hz: u32) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let dt = 1.0 / tick_rate_hz as f32;
        let waypoint_interval_ticks = WAYPOINT_INTERVAL_SECS * tick_rate_hz as u64;
        let velocity = Self::sample_velocity(&mut rng);
        Self {
            tick: 0,
            dt,
            waypoint_interval_ticks,
            next_waypoint_tick: waypoint_interval_ticks,
            state: EntityState::new([0.0, 0.0, 0.0], velocity),
            rng,
        }
    }

    fn sample_velocity(rng: &mut StdRng) -> [f32; 3] {
        [
            rng.gen_range(-MAX_SPEED..=MAX_SPEED),
            rng.gen_range(-MAX_SPEED..=MAX_SPEED),
            rng.gen_range(-MAX_SPEED..=MAX_SPEED),
        ]
    }

    /// Current ground-truth state without advancing the tick.
    pub fn state(&self) -> EntityState {
        self.state
    }

    /// Advances the world by one tick and returns the resulting snapshot.
    pub fn step(&mut self) -> WorldSnapshot {
        if self.tick >= self.next_waypoint_tick {
            self.state.velocity = Self::sample_velocity(&mut self.rng);
            self.next_waypoint_tick = self.tick + self.waypoint_interval_ticks;
        }

        for axis in 0..3 {
            self.state.position[axis] += self.state.velocity[axis] * self.dt;
        }
        self.tick += 1;

        WorldSnapshot {
            tick: self.tick,
            state: self.state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_velocity_integrates_linearly() {
        let tick_rate_hz = 60;
        let mut world = AuthoritativeWorld::new(42, tick_rate_hz);
        let v0 = world.state().velocity;
        let dt = 1.0 / tick_rate_hz as f32;

        // Stay well inside the first waypoint interval so velocity is held
        // constant for the whole run.
        let n = 50u64;
        for _ in 0..n {
            world.step();
        }

        let expected = [
            v0[0] * n as f32 * dt,
            v0[1] * n as f32 * dt,
            v0[2] * n as f32 * dt,
        ];
        let actual = world.state().position;
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 1e-3,
                "axis {axis}: expected {}, got {}",
                expected[axis],
                actual[axis]
            );
        }
    }

    #[test]
    fn tick_counter_advances_by_one_per_step() {
        let mut world = AuthoritativeWorld::new(7, 60);
        assert_eq!(world.tick, 0);
        let snap = world.step();
        assert_eq!(snap.tick, 1);
        assert_eq!(world.tick, 1);
    }

    #[test]
    fn same_seed_produces_identical_trajectory() {
        let mut a = AuthoritativeWorld::new(99, 60);
        let mut b = AuthoritativeWorld::new(99, 60);
        for _ in 0..500 {
            let sa = a.step();
            let sb = b.step();
            assert_eq!(sa.state.position, sb.state.position);
            assert_eq!(sa.state.velocity, sb.state.velocity);
        }
    }

    #[test]
    fn velocity_changes_after_waypoint_interval() {
        let tick_rate_hz = 60;
        let mut world = AuthoritativeWorld::new(5, tick_rate_hz);
        let v0 = world.state().velocity;
        // Advance past the first waypoint interval (1s @ 60Hz = 60 ticks).
        for _ in 0..70 {
            world.step();
        }
        assert_ne!(world.state().velocity, v0);
    }
}
