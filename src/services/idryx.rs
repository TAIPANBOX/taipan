//! Idryx (identity/access plane). `serve` loads its graph once at startup —
//! it does not tail the bus — so it is started with `--load
//! tokenfuse:<events dir>/tokenfuse.ndjson`, mirroring
//! `bank-in-a-box/run.sh`'s `idryx detect --load tokenfuse:...` but as a
//! long-running server. That file is guaranteed to exist (even if empty,
//! which parses as zero identities/events — see `Idryx.../tokenfuse.Load`)
//! because `commands::up` touches it before any service starts. Idryx's API
//! has no auth of its own (loopback design, 07 §4.4; its default :8080
//! collides with Cloud, hence the `--addr` remap to :8081).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::buildutil::is_stale;
use crate::descriptor::ServiceEntry;
use crate::home::TaipanHome;
use crate::procutil::{self, StopSignal};
use crate::services::StartedService;
use crate::workspace::Workspace;

pub const PORT: u16 = 8081;
const HEALTH_PATH: &str = "/healthz";

pub fn ensure_binary(workspace: &Workspace, home: &TaipanHome) -> Result<PathBuf> {
    let repo = workspace.find_repo("idryx", &["Idryx", "idryx"])?;
    let marker = home.bin_dir().join(".marker-idryx");
    let bin = home.bin_dir().join("idryx");

    if bin.is_file() && !is_stale(&marker, &repo, &["go"], &["go.mod", "go.sum"]) {
        tracing::info!(repo = %repo.display(), "idryx: up to date, skipping build");
        return Ok(bin);
    }

    tracing::info!(repo = %repo.display(), "idryx: building");
    std::fs::create_dir_all(home.bin_dir())
        .with_context(|| format!("create directory {}", home.bin_dir().display()))?;
    let status = Command::new("go")
        .args(["build", "-o"])
        .arg(&bin)
        .arg("./cmd/idryx")
        .current_dir(&repo)
        .status()
        .context("run `go build` for idryx")?;
    if !status.success() {
        anyhow::bail!("go build for idryx failed ({status})");
    }
    std::fs::write(&marker, b"")
        .with_context(|| format!("write build marker {}", marker.display()))?;
    tracing::info!(bin = %bin.display(), "idryx: built");
    Ok(bin)
}

pub fn start(
    bin: &Path,
    tokenfuse_events_path: &Path,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    let addr = format!("127.0.0.1:{PORT}");
    let load_spec = format!("tokenfuse:{}", tokenfuse_events_path.display());
    let args = vec![
        "serve".to_string(),
        "--addr".to_string(),
        addr.clone(),
        "--load".to_string(),
        load_spec,
    ];

    let spawned =
        procutil::spawn_process("idryx", bin, &args, &[], None, log_path, StopSignal::Term)
            .context("spawn idryx")?;
    tracing::info!(service = "idryx", pid = spawned.pid, addr = %addr, "spawned, waiting for /healthz");

    if let Err(e) =
        crate::health::wait_healthy(PORT, HEALTH_PATH, spawned.pid, healthz_timeout, log_path)
    {
        let _ = procutil::stop_group(spawned.pid, spawned.stop_signal, Duration::from_secs(10));
        return Err(e.context("idryx did not become healthy"));
    }
    tracing::info!(service = "idryx", pid = spawned.pid, "healthy");

    Ok(StartedService {
        spawned,
        entry: ServiceEntry {
            url: format!("http://{addr}"),
            mode: None,
        },
    })
}
