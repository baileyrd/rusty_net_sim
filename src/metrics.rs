//! Structured per-tick metrics output: one JSON Lines record per
//! (tick, client), for offline analysis (a notebook, a quick script, a
//! spreadsheet — deliberately not this crate's job).

use serde::Serialize;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// One (tick, client) reconciliation record.
#[derive(Serialize)]
struct MetricsRecord<'a> {
    tick: u64,
    client: &'a str,
    residual: f32,
    blend_frames: u8,
    pop: bool,
    /// Phase 2: distance between this client's peer-informed reconciled
    /// position (via `PeerPublishSource`) and its server-informed one
    /// (via `ServerOnlySource`) at the same tick. 0.0 before either has
    /// reconciled at least once.
    peer_divergence: f32,
}

/// Writes one JSON Lines record per (tick, client) to a file.
pub struct MetricsRecorder {
    writer: BufWriter<File>,
}

impl MetricsRecorder {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Appends one record. `pop` marks whether this reconciliation was
    /// aggressive enough to be a visible correction pop rather than a
    /// smooth blend. `peer_divergence` is the phase 2 peer-vs-server gap
    /// (see `MetricsRecord`).
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        tick: u64,
        client: &str,
        residual: f32,
        blend_frames: u8,
        pop: bool,
        peer_divergence: f32,
    ) -> io::Result<()> {
        let record = MetricsRecord {
            tick,
            client,
            residual,
            blend_frames,
            pop,
            peer_divergence,
        };
        serde_json::to_writer(&mut self.writer, &record)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufRead, BufReader};

    #[test]
    fn writes_one_parseable_jsonl_record_per_call() {
        let path = std::env::temp_dir().join(format!(
            "rusty_net_sim_metrics_test_{}.jsonl",
            std::process::id()
        ));

        {
            let mut recorder = MetricsRecorder::create(&path).unwrap();
            for tick in 0..5u64 {
                for client in ["c1", "c2"] {
                    recorder
                        .record(tick, client, tick as f32 * 1.5, 4, tick % 2 == 0, tick as f32 * 0.5)
                        .unwrap();
                }
            }
            recorder.flush().unwrap();
        }

        let file = fs::File::open(&path).unwrap();
        let lines: Vec<String> = BufReader::new(file)
            .lines()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(lines.len(), 5 * 2);

        for line in &lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(value.get("tick").is_some());
            assert!(value.get("client").is_some());
            assert!(value.get("residual").is_some());
            assert!(value.get("blend_frames").is_some());
            assert!(value.get("pop").is_some());
            assert!(value.get("peer_divergence").is_some());
        }

        fs::remove_file(&path).unwrap();
    }
}
