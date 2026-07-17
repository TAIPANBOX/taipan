//! The TokenFuse gateway (money-plane enforcement proxy). No auth of its own
//! (loopback design, 07 §4.1) — bound to 127.0.0.1 only.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::descriptor::ServiceEntry;
use crate::procutil::{self, StopSignal};
use crate::services::StartedService;

pub const PORT: u16 = 4100;
const HEALTH_PATH: &str = "/healthz";

#[allow(clippy::too_many_arguments)]
pub fn start(
    bin: &Path,
    mode: &str,
    events_path: &Path,
    data_dir: &Path,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("create {}", data_dir.display()))?;

    let addr = format!("127.0.0.1:{PORT}");
    let envs = vec![
        ("TOKENFUSE_ADDR".to_string(), addr.clone()),
        ("TOKENFUSE_MODE".to_string(), mode.to_string()),
        (
            "TOKENFUSE_EVENTS_PATH".to_string(),
            events_path.display().to_string(),
        ),
        (
            "TOKENFUSE_DATA_DIR".to_string(),
            data_dir.display().to_string(),
        ),
    ];

    let spawned =
        procutil::spawn_process("gateway", bin, &[], &envs, None, log_path, StopSignal::Int)?;
    tracing::info!(service = "gateway", pid = spawned.pid, addr = %addr, mode, "spawned, waiting for /healthz");

    if let Err(e) =
        crate::health::wait_healthy(PORT, HEALTH_PATH, spawned.pid, healthz_timeout, log_path)
    {
        // Self-cleanup: this process never became healthy, so it must not be
        // left running for the caller to discover as an orphan later.
        let _ = procutil::stop_group(spawned.pid, spawned.stop_signal, Duration::from_secs(10));
        return Err(e.context("gateway did not become healthy"));
    }
    tracing::info!(service = "gateway", pid = spawned.pid, "healthy");

    Ok(StartedService {
        spawned,
        entry: ServiceEntry {
            url: format!("http://{addr}"),
            mode: Some(mode.to_string()),
        },
    })
}
