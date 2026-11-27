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
//!     <name>.keys.json            dev bearer keys (0600), referenced, not embedded, by the descriptor
//!     <name>.wardryx-policy.yaml  demo Wardryx policy (only if --with wardryx)
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
        self.environments_dir().join(format!("{name}.key.json"))
    }

    /// The demo Wardryx policy file `taipan up --with wardryx` seeds at `up`
    /// time (see `services::wardryx::write_demo_policy`), named alongside the
    /// other per-environment files this module already owns.
    pub fn wardryx_policy_path(&self, name: &str) -> PathBuf {
        self.environments_dir()
            .join(format!("{name}.wardryx-policy.yaml"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(root: &str) -> TaipanHome {
        TaipanHome {
            root: PathBuf::from(root),
        }
    }

    #[test]
    fn every_path_sits_where_the_layout_says() {
        // The descriptor path is the one Genaryx watches; the rest are where
        // `down` looks for what `up` wrote. A moved path is silent on both.
        let h = at("/h/.taipan");
        assert_eq!(h.bin_dir(), PathBuf::from("/h/.taipan/bin"));
        assert_eq!(h.events_dir(), PathBuf::from("/h/.taipan/events"));
        assert_eq!(
            h.environments_dir(),
            PathBuf::from("/h/.taipan/environments")
        );
        assert_eq!(
            h.descriptor_path("dev"),
            PathBuf::from("/h/.taipan/environments/dev.json")
        );
        assert_eq!(
            h.pidfile_path("dev"),
            PathBuf::from("/h/.taipan/environments/dev.pid.json")
        );
        assert_eq!(
            h.keyfile_path("dev"),
            PathBuf::from("/h/.taipan/environments/dev.keys.json")
        );
        assert_eq!(
            h.wardryx_policy_path("dev"),
            PathBuf::from("/h/.taipan/environments/dev.wardryx-policy.yaml")
        );
        assert_eq!(
            h.logs_dir("dev"),
            PathBuf::from("/h/.taipan/environments/dev.logs")
        );
        assert_eq!(
            h.traces_dir("dev", "gateway"),
            PathBuf::from("/h/.taipan/environments/dev.traces/gateway")
        );
    }

    #[test]
    fn base_dirs_are_created_and_a_blocked_one_is_an_error() {
        let root = std::env::temp_dir().join(format!(
            "taipan-home-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random suffix")
        ));
        let home = TaipanHome { root: root.clone() };
        home.ensure_base_dirs().expect("create base dirs");
        for d in [home.bin_dir(), home.events_dir(), home.environments_dir()] {
            assert!(d.is_dir(), "{} was not created", d.display());
        }
        home.ensure_base_dirs()
            .expect("creating them twice is fine");

        std::fs::remove_dir_all(home.events_dir()).expect("remove events dir");
        std::fs::write(home.events_dir(), b"a file in the way").expect("block events dir");
        let err = home
            .ensure_base_dirs()
            .expect_err("a file where a dir goes");
        assert!(format!("{err:#}").contains("events"), "{err:#}");
        let _ = std::fs::remove_dir_all(root);
    }
}
