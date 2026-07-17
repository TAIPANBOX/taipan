//! `~/.taipan` layout. One place that knows every path taipan reads or
//! writes under the user's home directory, so the rest of the codebase never
//! hand-builds one of these paths itself.
//!
//! ```text
//! ~/.taipan/
//!   bin/                          cached built service binaries + staleness markers
//!   events/                       one NDJSON file per service, shared by every environment
//!   environments/
//!     <name>.json                 descriptor (the file consumers auto-discover)
//!     <name>.pid.json             tracked PIDs for `taipan down`
//!     <name>.keys.json            dev bearer keys (0600) — referenced, not embedded, by the descriptor
//!     <name>.logs/<service>.log   stdout+stderr of each spawned process
//!     <name>.traces/<service>/    optional Parquet trace dirs (gateway only, today)
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct TaipanHome {
    pub root: PathBuf,
}

impl TaipanHome {
    /// Resolve `~/.taipan` from `$HOME`. Deliberately the only place in the
    /// codebase that reads `$HOME` directly.
    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME environment variable is not set; taipan needs it to locate ~/.taipan")?;
        Ok(Self {
            root: home.join(".taipan"),
        })
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn events_dir(&self) -> PathBuf {
        self.root.join("events")
    }

    pub fn environments_dir(&self) -> PathBuf {
        self.root.join("environments")
    }

    pub fn logs_dir(&self, name: &str) -> PathBuf {
        self.environments_dir().join(format!("{name}.logs"))
    }

    pub fn traces_dir(&self, name: &str, service: &str) -> PathBuf {
        self.environments_dir()
            .join(format!("{name}.traces"))
            .join(service)
    }

    pub fn descriptor_path(&self, name: &str) -> PathBuf {
        self.environments_dir().join(format!("{name}.json"))
    }

    pub fn pidfile_path(&self, name: &str) -> PathBuf {
        self.environments_dir().join(format!("{name}.pid.json"))
    }

    pub fn keyfile_path(&self, name: &str) -> PathBuf {
        self.environments_dir().join(format!("{name}.keys.json"))
    }

    /// Create the directories every environment needs, regardless of which
    /// services it starts. Per-environment subdirectories (logs, traces) are
    /// created separately once `--name` is known to be valid.
    pub fn ensure_base_dirs(&self) -> Result<()> {
        for d in [self.bin_dir(), self.events_dir(), self.environments_dir()] {
            std::fs::create_dir_all(&d)
                .with_context(|| format!("create directory {}", d.display()))?;
        }
        Ok(())
    }
}
