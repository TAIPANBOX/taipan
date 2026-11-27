//! The pidfile `taipan up` writes per environment: every process it started,
//! so `taipan down` stops exactly those and only those, by process group,
//! never by scanning `ps`/`lsof` for a matching command line.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::procutil::{Spawned, StopSignal};
use crate::util::now_rfc3339;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcEntry {
    pub service: String,
    pub pid: i32,
    /// Always equal to `pid` (each process is spawned as its own group
    /// leader via `process_group(0)`), kept as an explicit named field so
    /// the pidfile is self-documenting for anyone reading it by hand.
    pub pgid: i32,
    pub stop_signal: String,
    pub started_at: String,
}

impl From<&Spawned> for ProcEntry {
    fn from(sp: &Spawned) -> Self {
        Self {
            service: sp.service.clone(),
            pid: sp.pid,
            pgid: sp.pid,
            stop_signal: sp.stop_signal.as_str().to_string(),
            started_at: now_rfc3339(),
        }
    }
}

impl ProcEntry {
    /// Parse the stored signal name back into `StopSignal`. Named distinctly
    /// from the `stop_signal` field (Rust allows the collision, but it reads
    /// confusingly at call sites), this is the field's typed counterpart.
    pub fn parsed_stop_signal(&self) -> StopSignal {
        StopSignal::parse(&self.stop_signal)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PidFile {
    pub name: String,
    pub processes: Vec<ProcEntry>,
}

impl PidFile {
    pub fn new(name: &str, processes: Vec<ProcEntry>) -> Self {
        Self {
            name: name.to_string(),
            processes,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let body =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize pidfile")?;
        std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "taipan-pidfile-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random suffix")
        ))
    }

    #[test]
    fn a_saved_pidfile_loads_back_entry_for_entry() {
        let dir = scratch();
        let path = dir.join("nested").join("env.pid.json");
        let spawned = Spawned {
            service: "gateway".to_string(),
            pid: 4242,
            stop_signal: StopSignal::Int,
            log_path: PathBuf::from("/tmp/gateway.log"),
        };
        let entry = ProcEntry::from(&spawned);
        assert_eq!(entry.pgid, entry.pid, "every process leads its own group");
        assert_eq!(entry.stop_signal, "SIGINT");

        PidFile::new("env", vec![entry]).save(&path).expect("save");
        let back = PidFile::load(&path).expect("load");

        assert_eq!(back.name, "env");
        assert_eq!(back.processes.len(), 1);
        assert_eq!(back.processes[0].service, "gateway");
        assert_eq!(back.processes[0].pid, 4242);
        assert_eq!(back.processes[0].parsed_stop_signal(), StopSignal::Int);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_or_corrupt_pidfile_names_the_file_in_its_error() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).expect("scratch");
        let missing = dir.join("missing.pid.json");
        let err = PidFile::load(&missing).expect_err("missing");
        assert!(format!("{err:#}").contains("missing.pid.json"), "{err:#}");

        let corrupt = dir.join("corrupt.pid.json");
        std::fs::write(&corrupt, b"{\"name\": 7}").expect("seed");
        let err = PidFile::load(&corrupt).expect_err("corrupt");
        let text = format!("{err:#}");
        assert!(
            text.starts_with("parse") && text.contains("corrupt.pid.json"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unwritable_path_is_an_error() {
        let dir = scratch();
        std::fs::create_dir_all(dir.join("env.pid.json")).expect("block with a dir");
        let err = PidFile::new("env", vec![])
            .save(&dir.join("env.pid.json"))
            .expect_err("a directory is in the way");
        assert!(format!("{err:#}").starts_with("write"), "{err:#}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
