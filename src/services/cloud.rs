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
/// (`"key:org:role,key:org:role"`, see `crate::keys`). `devkey` is
/// `UpArgs.devkey` verbatim, i.e. whether the operator actually passed
/// `taipan up --devkey`.
///
/// `TOKENFUSE_CLOUD_ALLOW_DEVKEY=1` is set if and only if `devkey` is true.
/// It used to be set unconditionally, on the reasoning that
/// `tokenfuse-cloud`'s own `parse_keys` only activates the literal `devkey`
/// bearer fallback when the parsed key map is empty (true: see
/// `commands::up::run`'s `--devkey` handling for why `keys_spec` is empty in
/// that mode and non-empty otherwise), so a minted, non-empty `keys_spec`
/// made the always-on flag harmless in practice. That reasoning holds only as
/// long as it stays true of a parser this crate does not own; setting the
/// flag here only when the operator asked for it removes the dependency on
/// another repo's behaviour rather than relying on it.
pub fn start(
    bin: &Path,
    keys_spec: &str,
    devkey: bool,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    let envs = build_envs(keys_spec, devkey);

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

/// The fixed environment `start` hands to the spawned binary. Pulled out of
/// `start` itself, which is otherwise all I/O (spawn, healthz poll), so the
/// `ALLOW_DEVKEY` decision has a seam to test without spawning a real
/// process.
fn build_envs(keys_spec: &str, devkey: bool) -> Vec<(String, String)> {
    let mut envs = vec![
        ("PORT".to_string(), PORT.to_string()),
        // Bind loopback only: this deploy and the on-box web shell reach the
        // money API at 127.0.0.1, and remote access goes through the tunnel,
        // never a raw open port. The binary already defaults to loopback; this
        // is explicit belt-and-suspenders.
        ("TOKENFUSE_CLOUD_HOST".to_string(), "127.0.0.1".to_string()),
        ("TOKENFUSE_CLOUD_KEYS".to_string(), keys_spec.to_string()),
    ];
    if devkey {
        envs.push(("TOKENFUSE_CLOUD_ALLOW_DEVKEY".to_string(), "1".to_string()));
    }
    envs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devkey_true_sets_allow_devkey() {
        let envs = build_envs("", true);
        assert!(
            envs.contains(&("TOKENFUSE_CLOUD_ALLOW_DEVKEY".to_string(), "1".to_string())),
            "devkey=true must set TOKENFUSE_CLOUD_ALLOW_DEVKEY=1, got {envs:?}"
        );
    }

    #[test]
    fn devkey_false_does_not_set_allow_devkey() {
        let envs = build_envs("abc123:default:admin,def456:default:viewer", false);
        assert!(
            !envs
                .iter()
                .any(|(k, _)| k == "TOKENFUSE_CLOUD_ALLOW_DEVKEY"),
            "devkey=false must not set TOKENFUSE_CLOUD_ALLOW_DEVKEY at all, got {envs:?}"
        );
    }

    #[test]
    fn keys_spec_is_passed_through_unchanged_either_way() {
        for devkey in [true, false] {
            let envs = build_envs("the-spec", devkey);
            assert!(
                envs.contains(&("TOKENFUSE_CLOUD_KEYS".to_string(), "the-spec".to_string())),
                "TOKENFUSE_CLOUD_KEYS must reach the process unchanged (devkey={devkey}), got {envs:?}"
            );
        }
    }
}
