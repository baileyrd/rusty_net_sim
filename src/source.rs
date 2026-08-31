//! Remote state sourcing: how a client obtains the authoritative state it
//! reconciles against. `ServerOnlySource` (baseline mode) is the only v1
//! implementation — it forwards whatever the client's `NetworkShim`
//! delivers each tick. `PeerPublishSource` is phase 2, out of scope here.

use crate::entity::EntityState;
use crate::shim::NetworkShim;

/// One authoritative snapshot as received by a client, tagged with the
/// server tick it was produced at.
#[derive(Clone, Copy, Debug)]
pub struct RemoteUpdate {
    pub tick: u64,
    pub state: EntityState,
}

/// Supplies a client with the latest remote state available as of a given
/// local tick.
pub trait RemoteStateSource {
    /// Returns the most recent update delivered by `tick`, if any arrived
    /// since the last call. Each update is returned exactly once.
    fn latest(&mut self, tick: u64) -> Option<RemoteUpdate>;
}

/// Baseline reconciliation mode: the client's only remote signal is
/// whatever the authoritative server sends it, subject to that client's
/// `NetworkShim`. If more than one snapshot becomes deliverable on the
/// same tick, only the most recent (by server tick) is kept — a client
/// reconciles against the freshest truth it has, not a stale one.
pub struct ServerOnlySource {
    shim: NetworkShim<RemoteUpdate>,
}

impl ServerOnlySource {
    pub fn new(shim: NetworkShim<RemoteUpdate>) -> Self {
        Self { shim }
    }

    /// Feeds a freshly produced authoritative snapshot into this source's
    /// network shim, to be delivered per its `LagProfile`.
    pub fn send(&mut self, send_tick: u64, update: RemoteUpdate) {
        self.shim.send(send_tick, update);
    }
}

impl RemoteStateSource for ServerOnlySource {
    fn latest(&mut self, tick: u64) -> Option<RemoteUpdate> {
        self.shim.poll(tick).into_iter().max_by_key(|u| u.tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconciler::RollbackReconciler;
    use crate::shim::LagProfile;

    #[test]
    fn reconciliation_fires_exactly_on_delivery_tick_not_before() {
        let profile = LagProfile {
            latency_ms: 50,
            jitter_ms: 0,
            loss_pct: 0.0,
        };
        let tick_rate_hz = 60;
        let shim: NetworkShim<RemoteUpdate> = NetworkShim::new(profile, tick_rate_hz, 7);
        let mut source = ServerOnlySource::new(shim);
        let mut reconciler = RollbackReconciler::new();

        let send_tick = 10u64;
        let sent_state = EntityState::new([1.0, 2.0, 3.0], [0.0; 3]);
        source.send(
            send_tick,
            RemoteUpdate {
                tick: send_tick,
                state: sent_state,
            },
        );

        let predicted = EntityState::new([9.0, 9.0, 9.0], [0.0; 3]);
        let mut reconcile_fired_at: Option<u64> = None;

        for tick in send_tick..=20 {
            if let Some(update) = source.latest(tick) {
                assert!(reconcile_fired_at.is_none(), "reconciled more than once");
                reconciler.reconcile(predicted, update.state);
                reconcile_fired_at = Some(tick);
            }
        }

        // 50ms @ 60Hz = round(50 / 16.667) = 3 ticks -> delivers at tick 13.
        assert_eq!(reconcile_fired_at, Some(13));
    }
}
