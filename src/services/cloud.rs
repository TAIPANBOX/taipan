//! TokenFuse Cloud (money-plane control API): summary/runs/agents/savings,
//! incidents, ES256 device-pairing mutations. Bound to `127.0.0.1` (loopback)
//! like every other service here: the binary now defaults to loopback, and we
//! set `TOKENFUSE_CLOUD_HOST` explicitly below for clarity. Remote access is via
//! the tunnel / on-box web shell, never a raw open port a scanner could reach.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::descriptor::ServiceEntry;
use crate::procutil::{self, StopSignal};
use crate::services::StartedService;

pub const PORT: u16 = 8080;
const HEALTH_PATH: &str = "/healthz";

/// `keys_spec` is a ready-made `TOKENFUSE_CLOUD_KEYS` value
/// (`"key:org:role,key:org:role"`, see `crate::keys`). `TOKENFUSE_CLOUD_ALLOW_DEVKEY=1`
/// is set alongside it as the documented dev-only fallback (task spec: "cloud
/// 8080 (ALLOW_DEVKEY on for dev)") - belt and suspenders, not a substitute
/// for the minted keys, which are what the descriptor actually references.
///
/// `keys_spec` may deliberately be empty: `commands::up`'s `--devkey` mode
/// passes `""` here on purpose, which (combined with `ALLOW_DEVKEY=1` above)
/// makes `tokenfuse-cloud`'s own `parse_keys` activate the literal `devkey`
/// bearer fallback instead of any minted key - see `commands::up::run` for
/// why.
pub fn start(
    bin: &Path,
    keys_spec: &str,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    let envs = vec![
        ("PORT".to_string(), PORT.to_string()),
        // Bind loopback only: this deploy and the on-box web shell reach the
        // money API at 127.0.0.1, and remote access goes through the tunnel,
        // never a raw open port. The binary already defaults to loopback; this
        // is explicit belt-and-suspenders.
        ("TOKENFUSE_CLOUD_HOST".to_string(), "127.0.0.1".to_string()),
        ("TOKENFUSE_CLOUD_KEYS".to_string(), keys_spec.to_string()),
        ("TOKENFUSE_CLOUD_ALLOW_DEVKEY".to_string(), "1".to_string()),
    ];

    let spawned =
        procutil::spawn_process("cloud", bin, &[], &envs, None, log_path, StopSignal::Term)
            .context("spawn cloud")?;
    tracing::info!(
        service = "cloud",
        pid = spawned.pid,
        port = PORT,
        "spawned, waiting for /healthz"
    );

    if let Err(e) =
        crate::health::wait_healthy(PORT, HEALTH_PATH, spawned.pid, healthz_timeout, log_path)
    {
        let _ = procutil::stop_group(spawned.pid, spawned.stop_signal, Duration::from_secs(10));
        return Err(e.context("cloud did not become healthy"));
    }
    tracing::info!(service = "cloud", pid = spawned.pid, "healthy");

    Ok(StartedService {
        spawned,
        entry: ServiceEntry {
            url: format!("http://127.0.0.1:{PORT}"),
            mode: None,
        },
    })
}
