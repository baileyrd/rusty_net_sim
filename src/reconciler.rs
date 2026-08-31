// Confidence-weighted rollback reconciliation for client-side prediction.
//
// Standard rollback netcode uses a fixed correction blend rate: every
// mispredicted entity gets smoothed (or snapped) the same way regardless
// of how good the local prediction has actually been lately.
//
// This borrows the "innovation-based adaptive estimation" idea from
// adaptive Kalman filtering / guidance-law adaptive gain: track a rolling
// confidence score per entity from recent prediction residuals, and scale
// correction aggressiveness by that score instead of using a fixed rate.
//
// Ported essentially as-is from the `adaptive_reconciliation.rs` design
// sketch. Confidence math, error ceiling, and the confidence -> blend_frames
// curve are first guesses meant to be tuned empirically once wired into a
// real client/server loop.

use crate::entity::EntityState;
use std::collections::VecDeque;

/// Tracks a rolling estimate of "how much do we trust our own prediction
/// for this entity right now," updated from residuals between predicted
/// and authoritative state each time a server snapshot arrives.
///
/// This is the same residual computation the replay-vs-simulation
/// divergence metric already needs for offline accuracy tuning, so the
/// two can share code: one consumer logs it for scoring the physics
/// engine, the other feeds it into this tracker for live reconciliation.
pub struct ConfidenceTracker {
    /// Exponentially-weighted moving average of recent position error.
    ewma_error: f32,
    /// Decay rate in (0, 1]; higher weights recent residuals more heavily.
    alpha: f32,
    /// Error magnitude (world units) above which confidence bottoms out.
    error_ceiling: f32,
}

impl ConfidenceTracker {
    pub fn new(alpha: f32, error_ceiling: f32) -> Self {
        Self {
            ewma_error: 0.0,
            alpha,
            error_ceiling,
        }
    }

    /// Feed a new residual (predicted vs. authoritative) into the tracker.
    pub fn update(&mut self, residual: f32) {
        self.ewma_error = self.alpha * residual + (1.0 - self.alpha) * self.ewma_error;
    }

    /// Confidence in [0, 1]. 1 = predictions have been tracking well,
    /// 0 = predictions have been consistently wrong.
    pub fn confidence(&self) -> f32 {
        (1.0 - (self.ewma_error / self.error_ceiling)).clamp(0.0, 1.0)
    }
}

/// How aggressively to reconcile a mispredicted entity back to the
/// authoritative state, derived from current confidence.
pub struct ReconciliationPolicy {
    /// Frames to smooth the correction over. Low confidence -> few frames
    /// (snap fast); high confidence -> more frames (smooth, avoid popping).
    pub blend_frames: u8,
}

impl ReconciliationPolicy {
    pub fn from_confidence(confidence: f32) -> Self {
        // First-pass linear map: confidence 0.0 -> snap over 1 frame,
        // confidence 1.0 -> smooth over 8 frames (~130ms @ 60Hz).
        // A real implementation would probably want a curve here rather
        // than linear, tuned against how correction "pop" actually feels.
        let blend_frames = 1 + (confidence * 7.0).round() as u8;
        Self { blend_frames }
    }
}

/// One entity's rollback reconciliation state across ticks.
pub struct RollbackReconciler {
    confidence: ConfidenceTracker,
    recent_residuals: VecDeque<f32>, // diagnostics / offline tuning
}

impl Default for RollbackReconciler {
    fn default() -> Self {
        Self::new()
    }
}

impl RollbackReconciler {
    pub fn new() -> Self {
        Self {
            confidence: ConfidenceTracker::new(0.2, /* error_ceiling */ 300.0),
            recent_residuals: VecDeque::with_capacity(64),
        }
    }

    /// Called when a server snapshot arrives for a tick the client already
    /// predicted locally. Returns the reconciliation policy to apply.
    pub fn reconcile(
        &mut self,
        predicted: EntityState,
        authoritative: EntityState,
    ) -> ReconciliationPolicy {
        let residual = predicted.position_error(&authoritative);

        self.confidence.update(residual);
        self.recent_residuals.push_back(residual);
        if self.recent_residuals.len() > 64 {
            self.recent_residuals.pop_front();
        }

        ReconciliationPolicy::from_confidence(self.confidence.confidence())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(pos: [f32; 3]) -> EntityState {
        EntityState::new(pos, [0.0, 0.0, 0.0])
    }

    #[test]
    fn confidence_stays_in_unit_range_under_repeated_large_errors() {
        let mut tracker = ConfidenceTracker::new(0.2, 300.0);
        for _ in 0..100 {
            tracker.update(10_000.0);
            let c = tracker.confidence();
            assert!((0.0..=1.0).contains(&c), "confidence out of range: {c}");
        }
        assert_eq!(tracker.confidence(), 0.0);
    }

    #[test]
    fn confidence_stays_in_unit_range_with_zero_error() {
        let mut tracker = ConfidenceTracker::new(0.2, 300.0);
        for _ in 0..100 {
            tracker.update(0.0);
            let c = tracker.confidence();
            assert!((0.0..=1.0).contains(&c), "confidence out of range: {c}");
        }
        assert_eq!(tracker.confidence(), 1.0);
    }

    #[test]
    fn blend_frames_stays_in_bounds_across_confidence_range() {
        for i in 0..=20 {
            let confidence = i as f32 / 20.0;
            let policy = ReconciliationPolicy::from_confidence(confidence);
            assert!(
                (1..=8).contains(&policy.blend_frames),
                "blend_frames out of range for confidence {confidence}: {}",
                policy.blend_frames
            );
        }
    }

    #[test]
    fn reconcile_returns_policy_derived_from_residual() {
        let mut reconciler = RollbackReconciler::new();
        let predicted = state([0.0, 0.0, 0.0]);
        let authoritative = state([0.0, 0.0, 0.0]);

        let policy = reconciler.reconcile(predicted, authoritative);
        // Zero residual on the very first sample drives confidence up from
        // its initial 1.0 baseline (ewma_error starts at 0), so blend_frames
        // should sit at the smooth end.
        assert_eq!(policy.blend_frames, 8);
        assert_eq!(reconciler.recent_residuals.back().copied(), Some(0.0));
    }
}
