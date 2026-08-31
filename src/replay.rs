//! Optional per-tick position recording, for animated playback rather than
//! offline reconciliation-quality analysis (that's `metrics.rs`'s job).
//! Kept as a separate file/format on purpose: different consumer, different
//! shape (one record per tick with every client nested, not one record per
//! (tick, client)) — Unix-style, one file per job.

use crate::entity::EntityState;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Top-down (x, y) projection of a 3D position — enough for a 2D replay
/// viewer; the z axis isn't needed there.
pub type Position2D = [f32; 2];

fn project(state: &EntityState) -> Position2D {
    [state.position[0], state.position[1]]
}

/// One tick's worth of positions: the authoritative server plus every
/// client's own canonical (server-informed) predicted position — the
/// "current frame snapshot" a replay viewer draws.
#[derive(Serialize)]
struct ReplayRecord {
    tick: u64,
    server: Position2D,
    clients: BTreeMap<String, Position2D>,
}

/// Writes one JSON Lines record per tick to a file.
pub struct ReplayRecorder {
    writer: BufWriter<File>,
}

impl ReplayRecorder {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Appends one tick's positions. `clients` pairs each client id with its
    /// current canonical predicted `EntityState`.
    pub fn record<'a>(
        &mut self,
        tick: u64,
        server: &EntityState,
        clients: impl IntoIterator<Item = (&'a str, &'a EntityState)>,
    ) -> io::Result<()> {
        let record = ReplayRecord {
            tick,
            server: project(server),
            clients: clients
                .into_iter()
                .map(|(id, state)| (id.to_string(), project(state)))
                .collect(),
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
    fn writes_one_parseable_jsonl_record_per_tick_with_every_client() {
        let path = std::env::temp_dir().join(format!(
            "rusty_net_sim_replay_test_{}.jsonl",
            std::process::id()
        ));

        let server = EntityState::new([1.0, 2.0, 3.0], [0.0; 3]);
        let c1 = EntityState::new([4.0, 5.0, 6.0], [0.0; 3]);
        let c2 = EntityState::new([7.0, 8.0, 9.0], [0.0; 3]);

        {
            let mut recorder = ReplayRecorder::create(&path).unwrap();
            for tick in 0..5u64 {
                recorder
                    .record(tick, &server, [("c1", &c1), ("c2", &c2)])
                    .unwrap();
            }
            recorder.flush().unwrap();
        }

        let file = fs::File::open(&path).unwrap();
        let lines: Vec<String> = BufReader::new(file)
            .lines()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(lines.len(), 5);

        for (i, line) in lines.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["tick"], i as u64);
            assert_eq!(value["server"], serde_json::json!([1.0, 2.0]));
            assert_eq!(value["clients"]["c1"], serde_json::json!([4.0, 5.0]));
            assert_eq!(value["clients"]["c2"], serde_json::json!([7.0, 8.0]));
        }

        fs::remove_file(&path).unwrap();
    }
}
