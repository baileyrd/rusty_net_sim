//! Kinematic entity state shared across the simulation.

/// Physical state of a tracked entity (ball, car) at a single tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityState {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
}

impl EntityState {
    pub fn new(position: [f32; 3], velocity: [f32; 3]) -> Self {
        Self { position, velocity }
    }

    /// Euclidean position error between this state and another.
    pub fn position_error(&self, other: &EntityState) -> f32 {
        let dx = self.position[0] - other.position[0];
        let dy = self.position[1] - other.position[1];
        let dz = self.position[2] - other.position[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_error_known_distance() {
        let a = EntityState::new([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let b = EntityState::new([3.0, 4.0, 0.0], [0.0, 0.0, 0.0]);
        assert_eq!(a.position_error(&b), 5.0);
    }

    #[test]
    fn position_error_zero_for_identical_states() {
        let a = EntityState::new([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]);
        assert_eq!(a.position_error(&a), 0.0);
    }

    #[test]
    fn position_error_is_symmetric() {
        let a = EntityState::new([1.0, 1.0, 1.0], [0.0, 0.0, 0.0]);
        let b = EntityState::new([4.0, 5.0, 1.0], [0.0, 0.0, 0.0]);
        assert_eq!(a.position_error(&b), b.position_error(&a));
    }
}
