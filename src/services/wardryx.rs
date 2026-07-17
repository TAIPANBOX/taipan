//! Wardryx (policy decision point). Started without `-policy`, so it runs
//! "zero policies, every request will be allowed" — deliberately permissive
//! for a v0 smoke stack; wiring real policies is console/Phase-2 work.

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

pub const PORT: u16 = 8090;
const HEALTH_PATH: &str = "/healthz";

pub fn ensure_binary(workspace: &Workspace, home: &TaipanHome) -> Result<PathBuf> {
    let repo = workspace.find_repo("wardryx", &["wardryx", "Wardryx"])?;
    let marker = home.bin_dir().join(".marker-wardryx");
    let bin = home.bin_dir().join("wardryx");

    if bin.is_file() && !is_stale(&marker, &repo, &["go"], &["go.mod", "go.sum"]) {
        tracing::info!(repo = %repo.display(), "wardryx: up to date, skipping build");
        return Ok(bin);
    }

    tracing::info!(repo = %repo.display(), "wardryx: building");
    std::fs::create_dir_all(home.bin_dir())
        .with_context(|| format!("create directory {}", home.bin_dir().display()))?;
    let status = Command::new("go")
        .args(["build", "-o"])
        .arg(&bin)
        .arg("./cmd/wardryx")
        .current_dir(&repo)
        .status()
        .context("run `go build` for wardryx")?;
    if !status.success() {
        anyhow::bail!("go build for wardryx failed ({status})");
    }
    std::fs::write(&marker, b"")
        .with_context(|| format!("write build marker {}", marker.display()))?;
    tracing::info!(bin = %bin.display(), "wardryx: built");
    Ok(bin)
}

pub fn start(
    bin: &Path,
    events_path: &Path,
    keys_spec: &str,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    let addr = format!("127.0.0.1:{PORT}");
    let args = vec![
        "serve".to_string(),
        "-addr".to_string(),
        addr.clone(),
        "-events".to_string(),
        events_path.display().to_string(),
    ];
    let envs = vec![("WARDRYX_KEYS".to_string(), keys_spec.to_string())];

    let spawned = procutil::spawn_process(
        "wardryx",
        bin,
        &args,
        &envs,
        None,
        log_path,
        StopSignal::Term,
    )
    .context("spawn wardryx")?;
    tracing::info!(service = "wardryx", pid = spawned.pid, addr = %addr, "spawned, waiting for /healthz");

    if let Err(e) =
        crate::health::wait_healthy(PORT, HEALTH_PATH, spawned.pid, healthz_timeout, log_path)
    {
        let _ = procutil::stop_group(spawned.pid, spawned.stop_signal, Duration::from_secs(10));
        return Err(e.context("wardryx did not become healthy"));
    }
    tracing::info!(service = "wardryx", pid = spawned.pid, "healthy");

    Ok(StartedService {
        spawned,
        entry: ServiceEntry {
            url: format!("http://{addr}"),
            mode: None,
        },
    })
}
