//! Scenario configuration: a TOML-defined seed, tick rate/count, output
//! path, and per-client network profiles.

use crate::shim::LagProfile;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One simulated client: an identifier plus the network conditions applied
/// to its connection.
#[derive(Deserialize)]
pub struct ClientConfig {
    pub id: String,
    #[serde(flatten)]
    pub lag_profile: LagProfile,
}

/// Full scenario definition loaded from a TOML file.
#[derive(Deserialize)]
pub struct ScenarioConfig {
    pub seed: u64,
    pub tick_rate_hz: u32,
    pub tick_count: u64,
    #[serde(default = "default_output_path")]
    pub output_path: PathBuf,
    /// Network conditions for the peer-to-peer ring link each client uses
    /// to publish its own reconciled state to its neighbor (phase 2,
    /// `PeerPublishSource`). Defaults to a fast, LAN-like link — the
    /// interesting comparison is a peer link that's *better* than the
    /// (often worse) server link each client separately has.
    #[serde(default = "default_peer_profile")]
    pub peer: LagProfile,
    /// Optional per-tick position recording (server + every client's
    /// canonical predicted position) for an animated replay viewer. Off by
    /// default — most scenario runs only care about `metrics.jsonl`.
    #[serde(default)]
    pub replay_path: Option<PathBuf>,
    pub clients: Vec<ClientConfig>,
}

fn default_output_path() -> PathBuf {
    PathBuf::from("metrics.jsonl")
}

fn default_peer_profile() -> LagProfile {
    LagProfile {
        latency_ms: 5,
        jitter_ms: 1,
        loss_pct: 0.0,
    }
}

/// Failure to load or parse a scenario TOML file.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "failed to read scenario config: {e}"),
            ConfigError::Parse(e) => write!(f, "failed to parse scenario config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl ScenarioConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        toml::from_str(&text).map_err(ConfigError::Parse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_scenario_shape() {
        let toml = r#"
            seed = 42
            tick_rate_hz = 60
            tick_count = 3600

            [[clients]]
            id = "c1"
            latency_ms = 20
            jitter_ms = 5
            loss_pct = 0.0

            [[clients]]
            id = "c2"
            latency_ms = 80
            jitter_ms = 25
            loss_pct = 1.0
        "#;
        let config: ScenarioConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.seed, 42);
        assert_eq!(config.tick_rate_hz, 60);
        assert_eq!(config.tick_count, 3600);
        assert_eq!(config.clients.len(), 2);
        assert_eq!(config.clients[0].id, "c1");
        assert_eq!(config.clients[0].lag_profile.latency_ms, 20);
        assert_eq!(config.clients[1].lag_profile.jitter_ms, 25);
        assert_eq!(config.output_path, PathBuf::from("metrics.jsonl"));
        assert_eq!(config.peer.latency_ms, 5);
        assert_eq!(config.peer.jitter_ms, 1);
        assert_eq!(config.peer.loss_pct, 0.0);
    }

    #[test]
    fn peer_profile_is_overridable() {
        let toml = r#"
            seed = 1
            tick_rate_hz = 60
            tick_count = 10

            [peer]
            latency_ms = 15
            jitter_ms = 4
            loss_pct = 2.0

            [[clients]]
            id = "c1"
            latency_ms = 0
            jitter_ms = 0
            loss_pct = 0.0
        "#;
        let config: ScenarioConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.peer.latency_ms, 15);
        assert_eq!(config.peer.jitter_ms, 4);
        assert_eq!(config.peer.loss_pct, 2.0);
    }

    #[test]
    fn output_path_is_overridable() {
        let toml = r#"
            seed = 1
            tick_rate_hz = 60
            tick_count = 10
            output_path = "out.jsonl"

            [[clients]]
            id = "c1"
            latency_ms = 0
            jitter_ms = 0
            loss_pct = 0.0
        "#;
        let config: ScenarioConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.output_path, PathBuf::from("out.jsonl"));
    }

    #[test]
    fn replay_path_defaults_to_none_and_is_settable() {
        let base = r#"
            seed = 1
            tick_rate_hz = 60
            tick_count = 10

            [[clients]]
            id = "c1"
            latency_ms = 0
            jitter_ms = 0
            loss_pct = 0.0
        "#;
        let config: ScenarioConfig = toml::from_str(base).unwrap();
        assert_eq!(config.replay_path, None);

        let with_replay = r#"
            seed = 1
            tick_rate_hz = 60
            tick_count = 10
            replay_path = "replay.jsonl"

            [[clients]]
            id = "c1"
            latency_ms = 0
            jitter_ms = 0
            loss_pct = 0.0
        "#;
        let config: ScenarioConfig = toml::from_str(with_replay).unwrap();
        assert_eq!(config.replay_path, Some(PathBuf::from("replay.jsonl")));
    }

    #[test]
    fn missing_required_field_fails_to_parse() {
        let toml = r#"
            seed = 1
            tick_rate_hz = 60
        "#;
        let result: Result<ScenarioConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }
}
