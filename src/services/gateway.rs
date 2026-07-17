//! The TokenFuse gateway (money-plane enforcement proxy). No caller-facing
//! auth of its own (loopback design, 07 §4.1): bound to 127.0.0.1 only.
//!
//! When `--with wardryx` is requested, this also wires the gateway to
//! consult Wardryx as its policy decision point: `TOKENFUSE_WARDRYX_MODE`,
//! `TOKENFUSE_WARDRYX_URL`, and `TOKENFUSE_WARDRYX_KEY` are the exact
//! `TOKENFUSE_WARDRYX_*` env contract `tokenfuse/crates/gateway/src/wardryx.rs`'s
//! `Wardryx::from_env` reads; nothing in tokenfuse itself needs to change for
//! this, it is purely config the gateway's child process is started with.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::descriptor::ServiceEntry;
use crate::procutil::{self, StopSignal};
use crate::services::StartedService;

pub const PORT: u16 = 4100;
const HEALTH_PATH: &str = "/healthz";

/// Generous compared to the gateway's own 50ms `TOKENFUSE_WARDRYX_TIMEOUT_MS`
/// default: taipan brings up a local dev/smoke-test stack, not a warmed
/// production deploy, so a slower first loopback call (cold connection pool,
/// a busy sandboxed host) should never trip the gateway's fail-open fallback
/// just because it missed a timeout tuned for the production hot path.
const WARDRYX_CALL_TIMEOUT_MS: &str = "2000";

#[allow(clippy::too_many_arguments)]
pub fn start(
    bin: &Path,
    mode: &str,
    events_path: &Path,
    data_dir: &Path,
    wardryx: Option<(&str, &str)>,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("create {}", data_dir.display()))?;

    let addr = format!("127.0.0.1:{PORT}");
    let mut envs = vec![
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
    // `wardryx` is `(base_url, bearer_key)`, pre-minted by `commands::up`
    // before either process starts (see that module's `WardryxWiring`).
    // `enforce` (not `shadow`) so a deny/hold actually short-circuits the
    // request instead of only reporting what it would have done. The key is
    // wardryx's own bearer, required because wardryx's `/v1/decide` sits
    // behind `requireAuth` (wardryx/internal/api): a viewer-role key is
    // enough, since the gateway never calls wardryx's admin-only endpoints.
    // `None` (no `--with wardryx`) leaves all four env vars unset, which is
    // exactly what keeps `Wardryx::from_env` the true off/no-op it is today.
    if let Some((url, key)) = wardryx {
        envs.push(("TOKENFUSE_WARDRYX_MODE".to_string(), "enforce".to_string()));
        envs.push(("TOKENFUSE_WARDRYX_URL".to_string(), url.to_string()));
        envs.push(("TOKENFUSE_WARDRYX_KEY".to_string(), key.to_string()));
        envs.push((
            "TOKENFUSE_WARDRYX_TIMEOUT_MS".to_string(),
            WARDRYX_CALL_TIMEOUT_MS.to_string(),
        ));
    }

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
