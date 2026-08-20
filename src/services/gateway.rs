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

/// Every environment variable the gateway's child process is started with,
/// split out of `start` for the same reason `cloud::build_envs` is: `start` is
/// otherwise all I/O (spawn, healthz poll), and the upstream-or-stub decision
/// below needs a seam that can be tested without spawning anything.
fn build_envs(
    addr: &str,
    mode: &str,
    events_path: &Path,
    data_dir: &Path,
    wardryx: Option<(&str, &str)>,
    upstream: Option<&str>,
) -> Vec<(String, String)> {
    let mut envs = vec![
        ("TOKENFUSE_ADDR".to_string(), addr.to_string()),
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
    // The gateway refuses to start with neither of these set, and it is right
    // to: without an upstream it answers from a built-in stub and meters a
    // fixed 1000 input / 500 output tokens as real spend, so both the answers
    // and the money would be invented. tokenfuse made the stub opt-IN on
    // 2026-07-25 (4b4b3fd, "gateway: refuse to start rather than invent
    // usage") and taipan set neither variable, so `taipan up` had been broken
    // against its own gateway for four weeks. Nothing said so, because no test
    // in this repository had ever run `up`.
    match upstream {
        Some(url) => envs.push(("TOKENFUSE_UPSTREAM".to_string(), url.to_string())),
        // The caller prints this in the summary an operator actually reads.
        // Setting it silently is the failure the tokenfuse commit above was
        // written about, one repository along.
        None => envs.push(("TOKENFUSE_ALLOW_STUB".to_string(), "1".to_string())),
    }

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

    envs
}

#[allow(clippy::too_many_arguments)]
pub fn start(
    bin: &Path,
    mode: &str,
    events_path: &Path,
    data_dir: &Path,
    wardryx: Option<(&str, &str)>,
    upstream: Option<&str>,
    log_path: &Path,
    healthz_timeout: Duration,
) -> Result<StartedService> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("create {}", data_dir.display()))?;

    let addr = format!("127.0.0.1:{PORT}");
    let envs = build_envs(&addr, mode, events_path, data_dir, wardryx, upstream);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn envs_for(upstream: Option<&str>) -> Vec<(String, String)> {
        build_envs(
            "127.0.0.1:4100",
            "enforce",
            Path::new("/tmp/events.ndjson"),
            Path::new("/tmp/traces"),
            None,
            upstream,
        )
    }

    fn value<'a>(envs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        envs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_gateway_never_starts_without_being_told_which_it_is() {
        // tokenfuse refuses to start with neither set, because a stub metering
        // a fixed 1000 input / 500 output tokens as real spend invents both
        // the answers and the money. taipan set neither from 2026-07-25 until
        // 2026-08-20, so `up` did not work at all and nothing said so.
        let stub = envs_for(None);
        assert_eq!(
            value(&stub, "TOKENFUSE_ALLOW_STUB"),
            Some("1"),
            "without --upstream the stub must be enabled EXPLICITLY, got {stub:?}"
        );
        assert_eq!(
            value(&stub, "TOKENFUSE_UPSTREAM"),
            None,
            "and no upstream may be invented for it"
        );

        let real = envs_for(Some("https://api.anthropic.com/v1/messages"));
        assert_eq!(
            value(&real, "TOKENFUSE_UPSTREAM"),
            Some("https://api.anthropic.com/v1/messages"),
            "--upstream must be passed through verbatim, got {real:?}"
        );
        assert_eq!(
            value(&real, "TOKENFUSE_ALLOW_STUB"),
            None,
            "and the stub must NOT also be enabled: a gateway with a real \
             upstream that can still fall back to invented usage is the exact \
             thing tokenfuse closed"
        );
    }

    #[test]
    fn wardryx_wiring_is_absent_unless_it_was_asked_for() {
        // `None` must leave all four TOKENFUSE_WARDRYX_* unset, which is what
        // keeps `Wardryx::from_env` a true no-op rather than a half-configured
        // policy client pointing nowhere.
        let off = envs_for(None);
        for k in [
            "TOKENFUSE_WARDRYX_MODE",
            "TOKENFUSE_WARDRYX_URL",
            "TOKENFUSE_WARDRYX_KEY",
            "TOKENFUSE_WARDRYX_TIMEOUT_MS",
        ] {
            assert_eq!(
                value(&off, k),
                None,
                "{k} must be unset without --with wardryx"
            );
        }

        let on = build_envs(
            "127.0.0.1:4100",
            "enforce",
            Path::new("/tmp/events.ndjson"),
            Path::new("/tmp/traces"),
            Some(("http://127.0.0.1:8090", "key:org:viewer")),
            None,
        );
        assert_eq!(value(&on, "TOKENFUSE_WARDRYX_MODE"), Some("enforce"));
        assert_eq!(
            value(&on, "TOKENFUSE_WARDRYX_URL"),
            Some("http://127.0.0.1:8090")
        );
        assert_eq!(value(&on, "TOKENFUSE_WARDRYX_KEY"), Some("key:org:viewer"));
    }
}
