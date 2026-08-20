//! Wardryx (policy decision point). `taipan up --with wardryx` seeds a demo
//! policy (see `DEMO_POLICY_YAML`/`write_demo_policy`) and a non-empty
//! `WARDRYX_APPROVAL_SECRET`, then passes both to `serve` below, so the
//! stack actually holds/denies something out of the box instead of running
//! with zero policies (the old default: every request allowed, and a hold
//! could never be granted since there was no secret to sign an
//! approval_token with).

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

/// Demo policy seeded at `up` time, scoped to the mockryx fire-drill
/// rehearsal identities (`agent://mockryx.local/*`) only, so it never
/// governs an operator's own agent traffic. Two rules:
/// - a small `require_human_above_usd` so a costly action holds for a human
///   rather than sailing through (matches mockryx's `approval-required`
///   scenario);
/// - a `deny_tool: [shell_exec]` so a tool-use request for it denies outright
///   (matches mockryx's `wardryx-denied-tool` scenario).
///
/// Both target the same glob: Wardryx's `Decide` checks deny_tool across
/// every matched policy before it ever reaches require_human_above_usd (see
/// wardryx/internal/pdp's rule-order doc comment), so a request that
/// declares `shell_exec` denies regardless of cost, and a request that
/// declares no tools only ever reaches the cost check. An operator replacing
/// this for real use points `-policy`/`WARDRYX_POLICY` at their own file or
/// directory; this one is not meant to survive past a smoke test.
const DEMO_POLICY_YAML: &str = r#"# Seeded by `taipan up --with wardryx`. Scoped to the mockryx fire-drill
# rehearsal identities only (agent://mockryx.local/*) so it never governs an
# operator's own agents. Replace this file, or point -policy/WARDRYX_POLICY
# elsewhere, for anything beyond a smoke test.
- name: taipan-demo-require-human-approval
  target: "agent://mockryx.local/*"
  require_human_above_usd: 1.0
- name: taipan-demo-deny-shell-exec
  target: "agent://mockryx.local/*"
  deny_tool:
    - shell_exec
"#;

/// Write the demo policy to `path`, truncating any previous content: a
/// fresh file every `up`, matching the events file's own touch-on-`up`
/// convention (never appended to, never left stale from an earlier run).
pub fn write_demo_policy(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    std::fs::write(path, DEMO_POLICY_YAML)
        .with_context(|| format!("write demo wardryx policy {}", path.display()))
}

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

#[allow(clippy::too_many_arguments)]
pub fn start(
    bin: &Path,
    events_path: &Path,
    keys_spec: &str,
    policy_path: &Path,
    approval_secret: &str,
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
        "-policy".to_string(),
        policy_path.display().to_string(),
    ];
    let envs = vec![
        ("WARDRYX_KEYS".to_string(), keys_spec.to_string()),
        (
            "WARDRYX_APPROVAL_SECRET".to_string(),
            approval_secret.to_string(),
        ),
    ];

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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("taipan-wardryx-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// The rule targets, read straight out of the seeded YAML. Parsed by hand
    /// rather than with a YAML crate: invariant 3 holds the dependency set at
    /// the declared crates, and this file's shape is fixed by the constant
    /// three lines above the parser.
    fn targets(yaml: &str) -> Vec<String> {
        yaml.lines()
            .filter_map(|l| l.trim().strip_prefix("target:"))
            .map(|v| v.trim().trim_matches('"').to_string())
            .collect()
    }

    #[test]
    fn the_demo_policy_targets_only_the_mockryx_rehearsal_identities() {
        // The safety property this policy's own doc comment claims: it "never
        // governs an operator's own agent traffic". A widened target would not
        // error, it would quietly start deciding on real requests.
        let found = targets(DEMO_POLICY_YAML);
        assert!(
            !found.is_empty(),
            "the demo policy must declare at least one target, found none"
        );
        for t in &found {
            assert_eq!(
                t, "agent://mockryx.local/*",
                "every rule must stay scoped to the rehearsal identities, found {t:?}"
            );
        }
    }

    #[test]
    fn the_demo_policy_holds_costly_actions_and_denies_shell_exec() {
        // Zero policies was the old default, and a stack that decides nothing
        // reads exactly like a stack that allows everything.
        assert!(
            DEMO_POLICY_YAML.contains("require_human_above_usd:"),
            "the demo policy must hold a costly action for a human"
        );
        assert!(
            DEMO_POLICY_YAML.contains("deny_tool:") && DEMO_POLICY_YAML.contains("shell_exec"),
            "the demo policy must deny shell_exec outright"
        );
    }

    #[test]
    fn seeding_the_policy_replaces_an_earlier_run_rather_than_appending() {
        // The second run is the real test. An appended file grows a duplicate
        // rule set on every `up`.
        let dir = scratch("replace");
        let path = dir.join("demo.wardryx-policy.yaml");
        std::fs::write(&path, b"- name: left-over-from-an-earlier-run\n").expect("seed old file");

        write_demo_policy(&path).expect("write demo policy");
        let first = std::fs::read_to_string(&path).expect("read policy");
        write_demo_policy(&path).expect("write demo policy again");
        let second = std::fs::read_to_string(&path).expect("read policy again");

        assert!(
            !second.contains("left-over-from-an-earlier-run"),
            "an earlier run's content must not survive: {second}"
        );
        assert_eq!(first, second, "a second seeding must not change the file");
        assert_eq!(
            second.matches("taipan-demo-deny-shell-exec").count(),
            1,
            "the rule must appear once, not once per `up`"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeding_the_policy_creates_the_directory_it_needs() {
        let dir = scratch("mkdir");
        let path = dir.join("never").join("created").join("policy.yaml");

        write_demo_policy(&path).expect("write demo policy into a missing directory");

        assert!(path.is_file(), "policy should exist at {}", path.display());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read policy"),
            DEMO_POLICY_YAML
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wardryx_port_is_the_one_the_readme_publishes() {
        // The README's port table is a promise to an operator, and the
        // descriptor consumers auto-discover is built from this constant.
        assert_eq!(PORT, 8090);
    }
}
