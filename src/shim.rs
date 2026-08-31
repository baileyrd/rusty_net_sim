//! Per-client network shim: simulates latency, jitter, and packet loss on
//! top of a fixed-tick, seeded-RNG delivery queue. No real sockets — this
//! is the "scheduled delivery queue" described in the design doc's
//! Determinism section.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::Deserialize;
use std::collections::VecDeque;

/// Network conditions applied to one client connection.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct LagProfile {
    pub latency_ms: u32,
    pub jitter_ms: u32,
    /// Percentage of messages dropped, in `[0, 100]`.
    pub loss_pct: f32,
}

struct Scheduled<T> {
    delivery_tick: u64,
    payload: T,
}

/// Simulates one client's network connection: each enqueued message is
/// either dropped (per `loss_pct`) or scheduled for delivery at
/// `send_tick + latency_ticks (+/- jitter_ticks)`. A shim owns its own
/// seeded RNG, so a given seed + `LagProfile` always produces the same
/// delivery schedule, independent of any other shim or system.
pub struct NetworkShim<T> {
    profile: LagProfile,
    latency_ticks: i64,
    jitter_ticks: i64,
    rng: StdRng,
    queue: VecDeque<Scheduled<T>>,
}

impl<T> NetworkShim<T> {
    /// `tick_rate_hz` converts the profile's millisecond fields into ticks.
    pub fn new(profile: LagProfile, tick_rate_hz: u32, seed: u64) -> Self {
        let ms_per_tick = 1000.0 / tick_rate_hz as f64;
        let latency_ticks = (profile.latency_ms as f64 / ms_per_tick).round() as i64;
        let jitter_ticks = (profile.jitter_ms as f64 / ms_per_tick).round() as i64;
        Self {
            profile,
            latency_ticks,
            jitter_ticks,
            rng: StdRng::seed_from_u64(seed),
            queue: VecDeque::new(),
        }
    }

    /// Enqueues `payload` sent at `send_tick`. Dropped per `loss_pct`;
    /// otherwise scheduled for delivery at `send_tick + latency +/- jitter`
    /// (delay floored at zero — a message can't arrive before it was sent).
    pub fn send(&mut self, send_tick: u64, payload: T) {
        if self.profile.loss_pct > 0.0 && self.rng.gen_range(0.0..100.0) < self.profile.loss_pct {
            return;
        }

        let jitter = if self.jitter_ticks > 0 {
            self.rng.gen_range(-self.jitter_ticks..=self.jitter_ticks)
        } else {
            0
        };
        let delay = (self.latency_ticks + jitter).max(0) as u64;
        let delivery_tick = send_tick + delay;

        let insert_at = self
            .queue
            .iter()
            .position(|scheduled| scheduled.delivery_tick > delivery_tick)
            .unwrap_or(self.queue.len());
        self.queue.insert(
            insert_at,
            Scheduled {
                delivery_tick,
                payload,
            },
        );
    }

    /// Pops and returns every payload whose delivery tick is `<= now`, in
    /// delivery order.
    pub fn poll(&mut self, now: u64) -> Vec<T> {
        let mut delivered = Vec::new();
        while let Some(front) = self.queue.front() {
            if front.delivery_tick <= now {
                delivered.push(self.queue.pop_front().unwrap().payload);
            } else {
                break;
            }
        }
        delivered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_jitter_zero_loss_delivers_at_fixed_offset() {
        let profile = LagProfile {
            latency_ms: 50,
            jitter_ms: 0,
            loss_pct: 0.0,
        };
        // 50ms @ 60Hz = round(50 / 16.667) = 3 ticks.
        let mut shim: NetworkShim<u32> = NetworkShim::new(profile, 60, 1);
        shim.send(10, 999);

        assert!(shim.poll(12).is_empty());
        assert_eq!(shim.poll(13), vec![999]);
    }

    #[test]
    fn full_loss_drops_everything() {
        let profile = LagProfile {
            latency_ms: 20,
            jitter_ms: 5,
            loss_pct: 100.0,
        };
        let mut shim: NetworkShim<u32> = NetworkShim::new(profile, 60, 2);
        for t in 0..50u64 {
            shim.send(t, t as u32);
        }

        assert!(shim.poll(10_000).is_empty());
    }

    #[test]
    fn same_seed_produces_identical_delivery_schedule() {
        let profile = LagProfile {
            latency_ms: 80,
            jitter_ms: 25,
            loss_pct: 10.0,
        };
        let mut a: NetworkShim<u32> = NetworkShim::new(profile, 60, 42);
        let mut b: NetworkShim<u32> = NetworkShim::new(profile, 60, 42);
        for t in 0..200u64 {
            a.send(t, t as u32);
            b.send(t, t as u32);
        }

        for now in 0..300u64 {
            assert_eq!(a.poll(now), b.poll(now));
        }
    }

    #[test]
    fn zero_loss_never_drops() {
        let profile = LagProfile {
            latency_ms: 10,
            jitter_ms: 10,
            loss_pct: 0.0,
        };
        let mut shim: NetworkShim<u32> = NetworkShim::new(profile, 60, 3);
        for t in 0..100u64 {
            shim.send(t, t as u32);
        }

        let mut delivered = shim.poll(10_000);
        delivered.sort_unstable();
        assert_eq!(delivered, (0..100u32).collect::<Vec<_>>());
    }
}
