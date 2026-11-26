//! `taipan up`: build/locate the requested services, start them in
//! dependency order (gateway and cloud are mandatory; wardryx/idryx are
//! opt-in via `--with` and degrade gracefully), wait for each one's
//! `/healthz`, then persist the pidfile, keyfile, and descriptor.
//!
//! Wardryx is a special case in that ordering: the gateway (part of the
//! mandatory pair, started first) needs Wardryx's URL and a bearer key up
//! front so it can be wired to consult Wardryx as its policy decision point
//! from the moment it comes up, but Wardryx itself is an opt-in `--with`
//! service that only starts afterward. `prepare_wardryx_wiring` resolves
//! that: it mints Wardryx's keys, its approval secret, and its demo policy
//! file before either process starts, and both the gateway and `start_wardryx`
//! are handed the exact same values, so they can never disagree about
//! Wardryx's address or which key is valid. If Wardryx then fails to come up,
//! the gateway keeps its config regardless: `TOKENFUSE_WARDRYX_FAILMODE`
//! defaults to `open` (an unreachable PDP resolves to allow), so an absent
//! Wardryx degrades to "the gateway behaves as if Wardryx were off," the same
//! graceful degradation every other `--with` service already gets, never a
//! stuck or half-enforcing gateway.
//!
//! Fail-closed rules this module enforces:
//! - A failure building/starting/health-checking the mandatory gateway or
//!   cloud aborts the whole command and stops anything already started -
//!   never a half-up stack.
//! - A failure in an opt-in `--with` service does NOT tear down an
//!   already-healthy mandatory pair; it is recorded in the descriptor's
//!   `unavailable` map and logged, so the environment still comes up usable
//!   and nothing is silently faked as green. This now also covers a failure
//!   preparing Wardryx's keys/secret/policy file (`prepare_wardryx_wiring`):
//!   that failure degrades `--with wardryx` exactly like a build or healthz
//!   failure would, rather than aborting the whole command.
//! - A failure persisting the pidfile/keyfile/descriptor itself rolls back
//!   every process started this run: an environment taipan cannot account
//!   for is not left running.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::cli::{Extra, UpArgs};
use crate::descriptor::{Descriptor, EventsSection, KeysSection};
use crate::home::TaipanHome;
use crate::keys::{self, DevKey, KeyFile};
use crate::pidfile::{PidFile, ProcEntry};
use crate::procutil::{self, Spawned};
use crate::services;
use crate::util::{hostname, now_rfc3339, random_hex, touch_file, validate_name};
use crate::workspace::Workspace;

const GATEWAY_MODES: [&str; 3] = ["shadow", "warn", "enforce"];

/// Where each service listens. Every real `up` uses [`Ports::STANDARD`], the
/// ports the README publishes and the descriptor advertises. It is a value
/// rather than four constants read deep inside each service so the whole of
/// `run_in` (spawn, healthz, rollback, persist) can be exercised by a test on
/// free ports, without fighting whatever an operator has on 4100 or 8080.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ports {
    gateway: u16,
    cloud: u16,
    wardryx: u16,
    idryx: u16,
}

impl Ports {
    const STANDARD: Ports = Ports {
        gateway: services::gateway::PORT,
        cloud: services::cloud::PORT,
        wardryx: services::wardryx::PORT,
        idryx: services::idryx::PORT,
    };
}

/// Wardryx's identity, resolved before either the gateway or Wardryx itself
/// actually starts. See the module doc comment for why this exists and how a
/// later Wardryx start failure is still handled gracefully.
struct WardryxWiring {
    url: String,
    admin: DevKey,
    viewer: DevKey,
    approval_secret: String,
    policy_path: std::path::PathBuf,
}

/// Mint Wardryx's dev keys and approval secret, and write its demo policy
/// file, all before any process starts. Kept infallible-looking but actually
/// fallible on purpose (`/dev/urandom`, directory creation, and the policy
/// write can all fail): the caller treats an `Err` here exactly like any
/// other `--with wardryx` failure, recording it in `unavailable` rather than
/// aborting the whole `up`.
fn prepare_wardryx_wiring(
    org: &str,
    home: &TaipanHome,
    name: &str,
    port: u16,
) -> Result<WardryxWiring> {
    let admin = keys::generate(org, "admin")?;
    let viewer = keys::generate(org, "viewer")?;
    // HMAC key wardryx signs/verifies approval_token with (WARDRYX_APPROVAL_SECRET).
    // 32 random bytes is ample entropy for a local/dev secret, matching the
    // 20-byte dev bearer keys `keys::generate` already mints.
    let approval_secret = random_hex(32)?;
    let policy_path = home.wardryx_policy_path(name);
    services::wardryx::write_demo_policy(&policy_path)?;
    Ok(WardryxWiring {
        url: format!("http://127.0.0.1:{port}"),
        admin,
        viewer,
        approval_secret,
        policy_path,
    })
}

pub fn run(args: UpArgs) -> Result<()> {
    validate_args(&args)?;
    let home = TaipanHome::discover()?;
    run_in(args, &home, Ports::STANDARD)
}

/// The checks that need nothing but the arguments, run before `$HOME` is
/// even looked at.
fn validate_args(args: &UpArgs) -> Result<()> {
    validate_name(&args.name)?;
    if !GATEWAY_MODES.contains(&args.gateway_mode.as_str()) {
        anyhow::bail!(
            "--gateway-mode must be one of shadow|warn|enforce (got {:?})",
            args.gateway_mode
        );
    }
    Ok(())
}

/// Everything `up` does once the arguments are known good and `~/.taipan` is
/// resolved. `run` passes [`Ports::STANDARD`].
fn run_in(args: UpArgs, home: &TaipanHome, ports: Ports) -> Result<()> {
    validate_args(&args)?;
    let healthz_timeout = Duration::from_secs(args.healthz_timeout_secs.max(1));

    home.ensure_base_dirs()?;

    let pidfile_path = home.pidfile_path(&args.name);
    refuse_if_already_up(&pidfile_path, &args.name)?;

    let workspace = match &args.workspace {
        Some(p) => Workspace::new(p.clone()),
        None => Workspace::current_dir()?,
    };

    let events_dir = home.events_dir();
    let logs_dir = home.logs_dir(&args.name);
    std::fs::create_dir_all(&logs_dir).with_context(|| format!("create {}", logs_dir.display()))?;

    let tokenfuse_events = events_dir.join("tokenfuse.ndjson");
    touch_file(&tokenfuse_events)?;

    let mut event_files: BTreeMap<String, String> = BTreeMap::new();
    event_files.insert("tokenfuse".to_string(), "tokenfuse.ndjson".to_string());

    let org = format!("taipan-{}", args.name);

    // `--devkey`: pass Cloud an EMPTY `TOKENFUSE_CLOUD_KEYS` spec instead of
    // minted keys, and `services::cloud::start` sets
    // `TOKENFUSE_CLOUD_ALLOW_DEVKEY=1` only because `args.devkey` is passed
    // through to it below; tokenfuse-cloud's own `parse_keys(spec,
    // allow_devkey)` only inserts the literal `devkey` fallback principal
    // (org=default, role=admin) when both are true: the flag is set AND the
    // parsed key map is empty. An empty spec here is what makes the second
    // half true (a non-empty spec would just mint real keys as before,
    // devkey or not); this flag is what makes the first half true, and now
    // only `--devkey` sets it. The keyfile then carries the literal string
    // "devkey" under both labels, so a console that auto-discovers this
    // environment reads a bearer Cloud genuinely accepts for pairing,
    // instead of a minted admin key that 401s at `/v1/pair/new` while devkey
    // is the only active credential (issue #20). Dev-only: never the
    // default, only behind this explicit flag.
    let (cloud_keys_spec, cloud_admin_secret, cloud_viewer_secret) = if args.devkey {
        (String::new(), "devkey".to_string(), "devkey".to_string())
    } else {
        let cloud_admin = keys::generate(&org, "admin")?;
        let cloud_viewer = keys::generate(&org, "viewer")?;
        // Server config gets the full token:org:role spec; the keyfile secret
        // gets the bare token, which is what a client sends as its bearer (the
        // server indexes by the bare token, so a full-spec secret 401s).
        let spec = format!("{},{}", cloud_admin.config_spec, cloud_viewer.config_spec);
        (spec, cloud_admin.token, cloud_viewer.token)
    };

    let mut secrets: BTreeMap<String, String> = BTreeMap::new();
    secrets.insert("cloud_admin".to_string(), cloud_admin_secret);
    secrets.insert("cloud_viewer".to_string(), cloud_viewer_secret);

    let mut keys_section = KeysSection {
        cloud_admin_ref: Some(keys::key_ref(&args.name, "cloud_admin")),
        cloud_viewer_ref: Some(keys::key_ref(&args.name, "cloud_viewer")),
        wardryx_admin_ref: None,
        wardryx_viewer_ref: None,
    };

    let mut started: Vec<Spawned> = Vec::new();
    let mut services_section = BTreeMap::new();
    let mut unavailable: BTreeMap<String, String> = BTreeMap::new();

    // Resolve Wardryx's keys/secret/policy before anything starts, so the
    // gateway (started below, ahead of Wardryx itself) can be wired to
    // consult it from the moment it comes up. See the module doc comment for
    // the full ordering rationale and the degrade-gracefully guarantee if
    // this fails or Wardryx never actually comes up.
    let wardryx_wiring = if args.with.contains(&Extra::Wardryx) {
        match prepare_wardryx_wiring(&org, home, &args.name, ports.wardryx) {
            Ok(w) => Some(w),
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::warn!(service = "wardryx", reason = %reason, "could not prepare keys/secret/policy; continuing without it");
                unavailable.insert("wardryx".to_string(), reason);
                None
            }
        }
    } else {
        None
    };

    // --- mandatory: tokenfuse gateway + cloud -----------------------------

    let (gateway_bin, cloud_bin) = services::tokenfuse_build::ensure_binaries(&workspace, home)
        .context("locate/build tokenfuse gateway + cloud")?;

    let gateway_log = logs_dir.join("gateway.log");
    let gateway_data_dir = home.traces_dir(&args.name, "gateway");
    match services::gateway::start(
        ports.gateway,
        &gateway_bin,
        &args.gateway_mode,
        &tokenfuse_events,
        &gateway_data_dir,
        wardryx_wiring
            .as_ref()
            .map(|w| (w.url.as_str(), w.viewer.token.as_str())),
        args.upstream.as_deref(),
        &gateway_log,
        healthz_timeout,
    ) {
        Ok(svc) => {
            started.push(svc.spawned);
            services_section.insert("gateway".to_string(), svc.entry);
        }
        Err(e) => {
            rollback(&started);
            return Err(e.context("gateway"));
        }
    }

    let cloud_log = logs_dir.join("cloud.log");
    match services::cloud::start(
        ports.cloud,
        &cloud_bin,
        &cloud_keys_spec,
        args.devkey,
        &cloud_log,
        healthz_timeout,
    ) {
        Ok(svc) => {
            started.push(svc.spawned);
            services_section.insert("cloud".to_string(), svc.entry);
        }
        Err(e) => {
            rollback(&started);
            return Err(e.context("cloud"));
        }
    }

    // --- optional: wardryx, idryx (--with) --------------------------------
    // Failures here degrade gracefully: the mandatory pair above stays up,
    // the failure is logged and recorded in the descriptor, `up` still
    // succeeds overall.

    if let Some(wiring) = &wardryx_wiring {
        match start_wardryx(
            ports.wardryx,
            &workspace,
            home,
            wiring,
            &events_dir,
            &logs_dir,
            healthz_timeout,
        ) {
            Ok(svc) => {
                started.push(svc.spawned);
                services_section.insert("wardryx".to_string(), svc.entry);
                event_files.insert("wardryx".to_string(), "wardryx.ndjson".to_string());
                secrets.insert("wardryx_admin".to_string(), wiring.admin.token.clone());
                secrets.insert("wardryx_viewer".to_string(), wiring.viewer.token.clone());
                keys_section.wardryx_admin_ref = Some(keys::key_ref(&args.name, "wardryx_admin"));
                keys_section.wardryx_viewer_ref = Some(keys::key_ref(&args.name, "wardryx_viewer"));
            }
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::warn!(service = "wardryx", reason = %reason, "unavailable on this box; continuing without it");
                unavailable.insert("wardryx".to_string(), reason);
            }
        }
    }

    if args.with.contains(&Extra::Idryx) {
        match start_idryx(
            ports.idryx,
            &workspace,
            home,
            &tokenfuse_events,
            &logs_dir,
            healthz_timeout,
        ) {
            Ok(svc) => {
                started.push(svc.spawned);
                services_section.insert("idryx".to_string(), svc.entry);
            }
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::warn!(service = "idryx", reason = %reason, "unavailable on this box; continuing without it");
                unavailable.insert("idryx".to_string(), reason);
            }
        }
    }

    // --- persist: pidfile, keyfile, descriptor -----------------------------
    // Anything unrecorded here is unaccountable, so any write failure rolls
    // back every process started this run rather than leaving it untracked.

    let pidfile = PidFile::new(&args.name, started.iter().map(ProcEntry::from).collect());
    if let Err(e) = pidfile.save(&pidfile_path) {
        tracing::error!(error = %e, "failed to write pidfile; rolling back everything started this run");
        rollback(&started);
        return Err(e.context("write pidfile"));
    }

    let keyfile_path = home.keyfile_path(&args.name);
    let descriptor_path = home.descriptor_path(&args.name);
    let keyfile = KeyFile::new(&args.name, secrets);
    if let Err(e) = keyfile.save(&keyfile_path) {
        tracing::error!(error = %e, "failed to write keyfile; rolling back everything started this run");
        rollback(&started);
        remove_pidfile_and_keyfile(&pidfile_path, &keyfile_path, &descriptor_path);
        return Err(e.context("write keyfile"));
    }

    let descriptor = Descriptor {
        name: args.name.clone(),
        created_at: now_rfc3339(),
        host: hostname(),
        services: services_section,
        events: EventsSection {
            dir: events_dir.display().to_string(),
            files: event_files,
        },
        keys: keys_section,
        unavailable,
        logs_dir: Some(logs_dir.display().to_string()),
    };
    if let Err(e) = descriptor.save(&descriptor_path) {
        tracing::error!(error = %e, "failed to write descriptor; rolling back everything started this run");
        rollback(&started);
        remove_pidfile_and_keyfile(&pidfile_path, &keyfile_path, &descriptor_path);
        return Err(e.context("write descriptor"));
    }

    print_summary(&args.name, &descriptor, home, args.upstream.as_deref());
    Ok(())
}

fn refuse_if_already_up(pidfile_path: &std::path::Path, name: &str) -> Result<()> {
    if !pidfile_path.is_file() {
        return Ok(());
    }
    match PidFile::load(pidfile_path) {
        Ok(existing)
            if existing
                .processes
                .iter()
                .any(|p| procutil::group_alive(p.pid)) =>
        {
            anyhow::bail!(
                "environment '{name}' already appears to be up (see {}); run `taipan down --name {name}` first",
                pidfile_path.display()
            );
        }
        Ok(_) => {
            tracing::warn!(path = %pidfile_path.display(), "stale pidfile with no live processes found; it will be overwritten");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(path = %pidfile_path.display(), error = %e, "could not parse existing pidfile; treating as stale and overwriting");
            Ok(())
        }
    }
}

fn start_wardryx(
    port: u16,
    workspace: &Workspace,
    home: &TaipanHome,
    wiring: &WardryxWiring,
    events_dir: &std::path::Path,
    logs_dir: &std::path::Path,
    healthz_timeout: Duration,
) -> Result<services::StartedService> {
    let bin = services::wardryx::ensure_binary(workspace, home)?;
    let events_path = events_dir.join("wardryx.ndjson");
    touch_file(&events_path)?;
    // Full spec for wardryx's key env; bare token for the keyfile secret
    // (wardryx's Go auth also indexes by the bare token, keys[parts[0]]).
    let spec = format!("{},{}", wiring.admin.config_spec, wiring.viewer.config_spec);
    let log_path = logs_dir.join("wardryx.log");
    services::wardryx::start(
        port,
        &bin,
        &events_path,
        &spec,
        &wiring.policy_path,
        &wiring.approval_secret,
        &log_path,
        healthz_timeout,
    )
}

fn start_idryx(
    port: u16,
    workspace: &Workspace,
    home: &TaipanHome,
    tokenfuse_events: &std::path::Path,
    logs_dir: &std::path::Path,
    healthz_timeout: Duration,
) -> Result<services::StartedService> {
    let bin = services::idryx::ensure_binary(workspace, home)?;
    let log_path = logs_dir.join("idryx.log");
    services::idryx::start(port, &bin, tokenfuse_events, &log_path, healthz_timeout)
}

/// Remove the pidfile, keyfile, and descriptor left behind when a later
/// persist step fails partway through `run`: an environment taipan cannot
/// fully account for must not leave state files on disk, at any
/// permissions, once the command is reporting failure. Shared by the
/// keyfile-failure and descriptor-failure branches so they cannot drift
/// apart again, and mirrors what `down.rs` removes on a normal teardown.
///
/// The keyfile-failure branch used to remove only the pidfile and leave the
/// keyfile (live Cloud/Wardryx bearer tokens) behind; that was fixed first.
/// The descriptor-failure branch then still left the descriptor itself
/// behind (Fable finding 3): `Descriptor::save` is `std::fs::write`, not an
/// atomic replace, so a failure partway through (ENOSPC, say) can leave a
/// truncated file at that path, which Genaryx auto-discovers as garbage
/// rather than as an absent environment. Passing `descriptor_path` here even
/// from the keyfile-failure branch, where no descriptor has been written
/// yet, is harmless: removing a path that was never created is the ordinary
/// NotFound case below, not an error. Best-effort throughout: the caller is
/// already unwinding with the real error.
fn remove_pidfile_and_keyfile(
    pidfile_path: &std::path::Path,
    keyfile_path: &std::path::Path,
    descriptor_path: &std::path::Path,
) {
    let _ = std::fs::remove_file(pidfile_path);
    let _ = std::fs::remove_file(keyfile_path);
    let _ = std::fs::remove_file(descriptor_path);
}

fn rollback(started: &[Spawned]) {
    for sp in started.iter().rev() {
        match procutil::stop_group(sp.pid, sp.stop_signal, Duration::from_secs(10)) {
            Ok(outcome) => {
                tracing::info!(service = %sp.service, pid = sp.pid, outcome = ?outcome, "rollback: stopped")
            }
            Err(e) => tracing::warn!(
                service = %sp.service,
                pid = sp.pid,
                log = %sp.log_path.display(),
                error = %e,
                "rollback: failed to stop; see its log for what it was doing"
            ),
        }
    }
}

fn print_summary(name: &str, descriptor: &Descriptor, home: &TaipanHome, upstream: Option<&str>) {
    println!();
    println!("taipan: environment '{name}' is up");
    for (svc, entry) in &descriptor.services {
        match &entry.mode {
            Some(mode) => println!("  {svc:<10} {}  (mode={mode})", entry.url),
            None => println!("  {svc:<10} {}", entry.url),
        }
    }
    if !descriptor.unavailable.is_empty() {
        println!("  unavailable:");
        for (svc, reason) in &descriptor.unavailable {
            println!("    {svc}: {reason}");
        }
    }
    println!("  events dir   {}", descriptor.events.dir);
    println!("  descriptor   {}", home.descriptor_path(name).display());
    println!("  pidfile      {}", home.pidfile_path(name).display());
    println!("  keyfile      {}", home.keyfile_path(name).display());
    // Said here, in the summary, and not only in a log line. The gateway is
    // answering from a built-in stub and metering a fixed 1000 input / 500
    // output tokens per call as spend. Those numbers travel: the descriptor
    // this command just wrote is what the Genaryx console auto-discovers, and
    // a person reads them there as money. An unlabelled stub is the exact
    // thing tokenfuse refused to start over.
    if upstream.is_none() {
        println!();
        println!("  !  NO UPSTREAM: the gateway is answering from its built-in stub.");
        println!("     Every call is metered at a fixed 1000 input / 500 output tokens,");
        println!("     so the spend shown here and in any console reading this");
        println!("     environment is INVENTED, not measured.");
        println!("     Pass --upstream <full provider endpoint> for real traffic.");
    }

    println!();
    println!("stop with: taipan down --name {name}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `run` itself is not unit-testable at the keyfile-failure seam: it
    /// discovers a workspace, builds/spawns real sibling binaries, and waits
    /// on real health checks, which is what `tests/end_to_end.rs` exercises
    /// instead (real ports, `#[ignore]`d, run only via `scripts/e2e.sh`).
    /// There is no injectable failure point to force `keyfile.save` to error
    /// from a unit test without running the whole command. What IS
    /// unit-testable is the cleanup both the keyfile-failure and the
    /// descriptor-failure branches call: this is the regression test for the
    /// keyfile-failure branch having removed only the pidfile and left the
    /// keyfile, carrying live bearer tokens, behind.
    #[test]
    fn remove_pidfile_and_keyfile_removes_both_when_present() {
        let dir = std::env::temp_dir().join(format!(
            "taipan-up-cleanup-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random dir suffix")
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let pidfile_path = dir.join("env.pid.json");
        let keyfile_path = dir.join("env.keys.json");
        let descriptor_path = dir.join("env.json");
        std::fs::write(&pidfile_path, b"{}").expect("write pidfile fixture");
        std::fs::write(&keyfile_path, b"{}").expect("write keyfile fixture");
        std::fs::write(&descriptor_path, b"{}").expect("write descriptor fixture");

        remove_pidfile_and_keyfile(&pidfile_path, &keyfile_path, &descriptor_path);

        assert!(!pidfile_path.exists(), "pidfile must be removed");
        assert!(
            !keyfile_path.exists(),
            "keyfile must be removed too, not just the pidfile"
        );
        assert!(
            !descriptor_path.exists(),
            "descriptor must be removed too (Fable finding 3: a partial \
             descriptor from a failed descriptor.save left behind is exactly \
             the garbage Genaryx auto-discovers)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The caller is already unwinding with the real error; cleanup must not
    /// panic when there is nothing left to clean up (an earlier partial run,
    /// or a keyfile save that failed before creating anything). Asserts the
    /// no-op is genuine (nothing appears at any of the three paths), rather
    /// than only proving the call does not panic (Fable finding 7).
    #[test]
    fn remove_pidfile_and_keyfile_is_a_no_op_when_neither_exists() {
        let dir = std::env::temp_dir().join(format!(
            "taipan-up-cleanup-noop-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random dir suffix")
        ));
        let pidfile_path = dir.join("env.pid.json");
        let keyfile_path = dir.join("env.keys.json");
        let descriptor_path = dir.join("env.json");

        remove_pidfile_and_keyfile(&pidfile_path, &keyfile_path, &descriptor_path);

        assert!(
            !pidfile_path.exists(),
            "no-op must not have created a pidfile"
        );
        assert!(
            !keyfile_path.exists(),
            "no-op must not have created a keyfile"
        );
        assert!(
            !descriptor_path.exists(),
            "no-op must not have created a descriptor"
        );
    }
}

/// A whole stack of fake services, for driving `run_in` (and `down`) end to
/// end without a sibling checkout, a compiler, or a fixed port.
///
/// Each fake is a shell script installed where `up` caches built binaries,
/// beside a fresh build marker so nothing is rebuilt. It records its own pid,
/// prints the arguments and the environment variables `up` is meant to hand
/// it, and then either `exec`s `sleep` (keeping that pid) or exits. Whether
/// it is "healthy" is decided by a listener this test owns on the port it is
/// told to use, so a fake never binds anything itself.
///
/// The reaper is what makes the rig honest. A child that dies unreaped is a
/// zombie, and `kill(-pgid, 0)` reads a zombie as alive, so without it every
/// stop would wait out its grace, escalate to SIGKILL, and report a corpse as
/// still running. In production the dead are reaped by init, because `up` has
/// exited long before `down` probes. Here the test process is the parent, so
/// it reaps exactly the pids the fakes recorded, and nothing else: a global
/// `waitpid(-1)` would steal children other tests are waiting on.
#[cfg(test)]
pub(crate) mod rig {
    use std::collections::HashSet;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::cli::{Extra, UpArgs};
    use crate::home::TaipanHome;
    use crate::procutil::{group_alive, stop_group, StopSignal};

    /// How a fake behaves once started.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fake {
        /// Keeps running (`exec sleep`) until signalled.
        Runs,
        /// Prints a complaint and exits 3 straight away.
        Exits,
    }

    /// A listener on a free port, for the life of the test process, that
    /// answers every request with the status line `status()` returns.
    fn answering_with(status: impl Fn() -> &'static str + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a free port");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let reply = format!(
                    "{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    status()
                );
                let _ = s.write_all(reply.as_bytes());
            }
        });
        port
    }

    /// A listener that always answers with `status`, whatever is running.
    pub fn answering(status: &'static str) -> u16 {
        answering_with(move || status)
    }

    /// A port nothing answers on: bound once to learn a free number, released.
    pub fn silent_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind a free port");
        l.local_addr().expect("local addr").port()
    }

    pub struct Rig {
        pub root: PathBuf,
        pub home: TaipanHome,
        pub workspace: PathBuf,
        pid_dir: PathBuf,
        seen: Arc<Mutex<HashSet<i32>>>,
        reaped: Arc<Mutex<HashSet<i32>>>,
        stop: Arc<AtomicBool>,
    }

    impl Rig {
        /// An empty home and a workspace holding empty `tokenfuse`, `wardryx`
        /// and `idryx` checkouts (nothing in them is newer than a marker, so a
        /// cached fake is never rebuilt).
        pub fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "taipan-rig-{label}-{}-{}",
                std::process::id(),
                crate::util::random_hex(4).expect("random suffix")
            ));
            let workspace = root.join("ws");
            for repo in ["tokenfuse", "wardryx", "idryx"] {
                std::fs::create_dir_all(workspace.join(repo)).expect("create sibling checkout");
            }
            let pid_dir = root.join("pids");
            std::fs::create_dir_all(&pid_dir).expect("create pid dir");
            let home = TaipanHome {
                root: root.join("home").join(".taipan"),
            };
            let rig = Rig {
                root,
                home,
                workspace,
                pid_dir,
                seen: Arc::new(Mutex::new(HashSet::new())),
                reaped: Arc::new(Mutex::new(HashSet::new())),
                stop: Arc::new(AtomicBool::new(false)),
            };
            rig.start_reaper();
            rig
        }

        fn start_reaper(&self) {
            let (dir, seen, stop) = (self.pid_dir.clone(), self.seen.clone(), self.stop.clone());
            let reaped = self.reaped.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                        let Ok(text) = std::fs::read_to_string(entry.path()) else {
                            continue;
                        };
                        let Ok(pid) = text.trim().parse::<i32>() else {
                            continue;
                        };
                        if seen.lock().expect("seen lock").insert(pid) {
                            let reaped = reaped.clone();
                            std::thread::spawn(move || {
                                let mut status: libc::c_int = 0;
                                // SAFETY: `pid` is a child this test process
                                // spawned (through `spawn_process`) and
                                // nothing else waits on it; `status` is a
                                // valid out-pointer for the call's duration.
                                unsafe { libc::waitpid(pid, &mut status, 0) };
                                reaped.lock().expect("reaped lock").insert(pid);
                            });
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }

        /// A port that answers `200 OK` once the fake `bin_name` has started
        /// and its pid is known to the reaper, and `503` before that. A
        /// service is healthy only after it exists, and a stop can then never
        /// race a fake the rig does not know about yet.
        pub fn healthy(&self, bin_name: &str) -> u16 {
            let pid_file = self.pid_dir.join(format!("{bin_name}.pid"));
            let seen = self.seen.clone();
            answering_with(move || {
                let known = std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|t| t.trim().parse::<i32>().ok())
                    .is_some_and(|pid| seen.lock().expect("seen lock").contains(&pid));
                if known {
                    "HTTP/1.1 200 OK"
                } else {
                    "HTTP/1.1 503 Service Unavailable"
                }
            })
        }

        /// Install a fake under `bin_name` with its build `marker`.
        pub fn install(&self, bin_name: &str, marker: &str, fake: Fake) {
            let bin_dir = self.home.bin_dir();
            std::fs::create_dir_all(&bin_dir).expect("create bin dir");
            let pid_file = self.pid_dir.join(format!("{bin_name}.pid"));
            // Everything is printed before the pid is recorded, so by the time
            // the rig knows a fake (and its port reads healthy) its log is
            // complete.
            let mut script = String::from("#!/bin/sh\necho \"args=$*\"\n");
            for var in [
                "TOKENFUSE_ADDR",
                "TOKENFUSE_MODE",
                "TOKENFUSE_UPSTREAM",
                "TOKENFUSE_ALLOW_STUB",
                "TOKENFUSE_WARDRYX_URL",
                "TOKENFUSE_WARDRYX_KEY",
                "TOKENFUSE_CLOUD_KEYS",
                "TOKENFUSE_CLOUD_ALLOW_DEVKEY",
                "PORT",
                "WARDRYX_KEYS",
            ] {
                script.push_str(&format!("echo \"{var}=${{{var}-<unset>}}\"\n"));
            }
            if fake == Fake::Exits {
                script.push_str(&format!("echo 'fake {bin_name} refusing to start'\n"));
            }
            script.push_str(&format!("echo $$ > '{}'\n", pid_file.display()));
            script.push_str(match fake {
                Fake::Runs => "exec sleep 60\n",
                Fake::Exits => "exit 3\n",
            });
            let path = bin_dir.join(bin_name);
            std::fs::write(&path, script).expect("write fake service");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make fake service executable");
            std::fs::write(bin_dir.join(marker), b"").expect("write build marker");
        }

        /// The money plane: gateway and cloud, both cached.
        pub fn install_money_plane(&self, gateway: Fake, cloud: Fake) {
            self.install("tokenfuse-gateway", ".marker-tokenfuse", gateway);
            self.install("tokenfuse-cloud", ".marker-tokenfuse", cloud);
        }

        /// The pid a fake recorded, once it has started.
        pub fn pid_of(&self, bin_name: &str) -> Option<i32> {
            std::fs::read_to_string(self.pid_dir.join(format!("{bin_name}.pid")))
                .ok()
                .and_then(|t| t.trim().parse().ok())
        }

        pub fn log(&self, name: &str, service: &str) -> String {
            let path = self.home.logs_dir(name).join(format!("{service}.log"));
            std::fs::read_to_string(path).unwrap_or_default()
        }

        pub fn args(&self, name: &str, with: Vec<Extra>) -> UpArgs {
            UpArgs {
                name: name.to_string(),
                with,
                workspace: Some(self.workspace.clone()),
                gateway_mode: "enforce".to_string(),
                healthz_timeout_secs: 1,
                devkey: false,
                upstream: None,
            }
        }

        pub fn json(&self, path: &Path) -> serde_json::Value {
            let body = std::fs::read_to_string(path).expect("read json file");
            serde_json::from_str(&body).expect("parse json file")
        }

        /// Every fake that started and whose group still exists. A reaped
        /// pid is dead by definition and is left out, because the kernel may
        /// already have handed that number to somebody else.
        pub fn alive(&self) -> Vec<i32> {
            let seen = self.seen.lock().expect("seen lock").clone();
            let reaped = self.reaped.lock().expect("reaped lock").clone();
            seen.into_iter()
                .filter(|p| !reaped.contains(p) && group_alive(*p))
                .collect()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            for pid in self.alive() {
                let _ = stop_group(pid, StopSignal::Term, Duration::from_secs(2));
            }
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod stack_tests {
    use super::rig::{answering, silent_port, Fake, Rig};
    use super::*;
    use crate::cli::DownArgs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn ports(gateway: u16, cloud: u16, wardryx: u16, idryx: u16) -> Ports {
        Ports {
            gateway,
            cloud,
            wardryx,
            idryx,
        }
    }

    fn healthy_ports(rig: &Rig) -> Ports {
        ports(
            rig.healthy("tokenfuse-gateway"),
            rig.healthy("tokenfuse-cloud"),
            rig.healthy("wardryx"),
            rig.healthy("idryx"),
        )
    }

    fn install_all(rig: &Rig) {
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        rig.install("wardryx", ".marker-wardryx", Fake::Runs);
        rig.install("idryx", ".marker-idryx", Fake::Runs);
    }

    fn down(rig: &Rig, name: &str) -> Result<()> {
        let args = DownArgs {
            name: name.to_string(),
        };
        crate::commands::down::run_in(&args, &rig.home, Duration::from_secs(5))
    }

    fn secret(keyfile: &serde_json::Value, label: &str) -> String {
        keyfile["secrets"][label]
            .as_str()
            .unwrap_or_else(|| panic!("keyfile has no secret {label}: {keyfile}"))
            .to_string()
    }

    #[test]
    fn the_standard_ports_are_the_published_ones() {
        assert_eq!(Ports::STANDARD, ports(4100, 8080, 8090, 8081));
    }

    #[test]
    fn up_starts_every_service_records_it_and_down_removes_all_of_it() {
        let rig = Rig::new("full");
        install_all(&rig);
        let p = healthy_ports(&rig);
        let name = "full";

        run_in(rig.args(name, vec![Extra::Wardryx, Extra::Idryx]), &rig.home, p)
            .expect("up with every service healthy");

        let descriptor = rig.json(&rig.home.descriptor_path(name));
        let services = descriptor["services"].as_object().expect("services map");
        let mut names: Vec<&str> = services.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["cloud", "gateway", "idryx", "wardryx"]);
        assert_eq!(
            descriptor["services"]["gateway"]["url"],
            format!("http://127.0.0.1:{}", p.gateway)
        );
        assert_eq!(descriptor["services"]["gateway"]["mode"], "enforce");
        assert_eq!(
            descriptor["services"]["cloud"]["url"],
            format!("http://127.0.0.1:{}", p.cloud)
        );
        assert_eq!(
            descriptor["services"]["wardryx"]["url"],
            format!("http://127.0.0.1:{}", p.wardryx)
        );
        assert_eq!(
            descriptor["services"]["idryx"]["url"],
            format!("http://127.0.0.1:{}", p.idryx)
        );
        assert!(
            descriptor.get("unavailable").is_none(),
            "nothing failed, so nothing may be listed as unavailable: {descriptor}"
        );
        assert_eq!(descriptor["events"]["files"]["wardryx"], "wardryx.ndjson");
        assert_eq!(
            descriptor["keys"]["wardryx_viewer_ref"],
            format!("taipan/{name}/wardryx_viewer")
        );

        // The keyfile: mode 0600, and the bearer the gateway was handed for
        // Wardryx is the very one recorded as Wardryx's viewer key. The two
        // processes are configured separately, so this is what proves they
        // agree on which key is valid.
        let keyfile_path = rig.home.keyfile_path(name);
        let mode = std::fs::metadata(&keyfile_path)
            .expect("keyfile metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "keyfile mode {mode:o}");
        let keyfile = rig.json(&keyfile_path);
        let viewer = secret(&keyfile, "wardryx_viewer");
        let admin = secret(&keyfile, "wardryx_admin");
        assert!(viewer.starts_with("tp_") && admin.starts_with("tp_"));
        assert_ne!(viewer, admin);
        assert!(secret(&keyfile, "cloud_admin").starts_with("tp_"));

        let gateway_log = rig.log(name, "gateway");
        assert!(gateway_log.contains(&format!("TOKENFUSE_WARDRYX_KEY={viewer}\n")));
        assert!(gateway_log.contains(&format!(
            "TOKENFUSE_WARDRYX_URL=http://127.0.0.1:{}\n",
            p.wardryx
        )));
        assert!(gateway_log.contains(&format!("TOKENFUSE_ADDR=127.0.0.1:{}\n", p.gateway)));
        assert!(gateway_log.contains("TOKENFUSE_ALLOW_STUB=1\n"));
        assert!(rig.log(name, "cloud").contains(&format!("PORT={}\n", p.cloud)));
        let wardryx_log = rig.log(name, "wardryx");
        assert!(wardryx_log.contains(&format!("args=serve -addr 127.0.0.1:{}", p.wardryx)));
        assert!(wardryx_log.contains(&format!("{admin}:taipan-{name}:admin")));
        assert!(rig
            .log(name, "idryx")
            .contains(&format!("args=serve --addr 127.0.0.1:{} --load tokenfuse:", p.idryx)));

        // The pidfile names every process, in start order, gateway on SIGINT.
        let pidfile = PidFile::load(&rig.home.pidfile_path(name)).expect("load pidfile");
        let order: Vec<&str> = pidfile.processes.iter().map(|e| e.service.as_str()).collect();
        assert_eq!(order, ["gateway", "cloud", "wardryx", "idryx"]);
        assert_eq!(pidfile.processes[0].stop_signal, "SIGINT");
        for entry in &pidfile.processes {
            assert_eq!(entry.pid, entry.pgid);
            assert!(procutil::group_alive(entry.pid), "{} is not running", entry.service);
        }
        assert_eq!(rig.alive().len(), 4);

        down(&rig, name).expect("down");
        assert!(rig.alive().is_empty(), "down left {:?} running", rig.alive());
        assert!(!rig.home.pidfile_path(name).exists());
        assert!(!rig.home.keyfile_path(name).exists());
        assert!(!rig.home.descriptor_path(name).exists());

        down(&rig, name).expect("a second down is a no-op, not an error");
    }

    #[test]
    fn a_second_up_is_refused_while_the_first_is_running() {
        let rig = Rig::new("twice");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        let p = healthy_ports(&rig);
        run_in(rig.args("twice", vec![]), &rig.home, p).expect("first up");
        let before = std::fs::read(rig.home.pidfile_path("twice")).expect("pidfile");

        let err = run_in(rig.args("twice", vec![]), &rig.home, p)
            .expect_err("a second up must not start a second copy");

        assert!(format!("{err:#}").contains("already appears to be up"), "{err:#}");
        let after = std::fs::read(rig.home.pidfile_path("twice")).expect("pidfile");
        assert_eq!(before, after, "the refused up must not touch the pidfile");
        assert_eq!(rig.alive().len(), 2, "still exactly one gateway and one cloud");
        down(&rig, "twice").expect("down");
        assert!(rig.alive().is_empty());
    }

    #[test]
    fn a_stale_or_unreadable_pidfile_does_not_block_a_fresh_up() {
        for (label, body) in [
            ("stale", r#"{"name":"x","processes":[]}"#),
            ("garbage", "this is not json"),
        ] {
            let rig = Rig::new(label);
            rig.install_money_plane(Fake::Runs, Fake::Runs);
            rig.home.ensure_base_dirs().expect("base dirs");
            std::fs::write(rig.home.pidfile_path(label), body).expect("seed pidfile");

            run_in(rig.args(label, vec![]), &rig.home, healthy_ports(&rig))
                .unwrap_or_else(|e| panic!("{label}: a dead pidfile must not block up: {e:#}"));

            let pidfile = PidFile::load(&rig.home.pidfile_path(label)).expect("load pidfile");
            assert_eq!(pidfile.processes.len(), 2, "{label}: overwritten with the new run");
            down(&rig, label).expect("down");
        }
    }

    #[test]
    fn a_gateway_that_dies_fails_up_and_leaves_nothing_behind() {
        let rig = Rig::new("gwdies");
        rig.install_money_plane(Fake::Exits, Fake::Runs);
        let p = ports(silent_port(), silent_port(), silent_port(), silent_port());

        let err = run_in(rig.args("gwdies", vec![]), &rig.home, p)
            .expect_err("a gateway that never answers must fail up");

        let text = format!("{err:#}");
        assert!(text.starts_with("gateway"), "{text}");
        assert!(text.contains("process exited before answering"), "{text}");
        assert!(
            text.contains("fake tokenfuse-gateway refusing to start"),
            "the error must carry the service's own log tail: {text}"
        );
        assert!(rig.pid_of("tokenfuse-cloud").is_none(), "cloud must not have started");
        assert!(rig.alive().is_empty());
        assert!(!rig.home.pidfile_path("gwdies").exists());
        assert!(!rig.home.descriptor_path("gwdies").exists());
    }

    #[test]
    fn a_cloud_that_never_answers_rolls_back_the_healthy_gateway() {
        let rig = Rig::new("cloudhangs");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        let p = ports(
            rig.healthy("tokenfuse-gateway"),
            silent_port(),
            silent_port(),
            silent_port(),
        );

        let err = run_in(rig.args("cloudhangs", vec![]), &rig.home, p)
            .expect_err("a cloud that never answers must fail up");

        let text = format!("{err:#}");
        assert!(text.starts_with("cloud"), "{text}");
        assert!(text.contains("timed out"), "{text}");
        assert!(rig.pid_of("tokenfuse-gateway").is_some(), "the gateway did start");
        assert!(
            rig.alive().is_empty(),
            "never a half-up stack: {:?} still running",
            rig.alive()
        );
        assert!(!rig.home.pidfile_path("cloudhangs").exists());
    }

    #[test]
    fn optional_services_that_fail_are_named_and_the_money_plane_stays_up() {
        let rig = Rig::new("degraded");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        rig.install("wardryx", ".marker-wardryx", Fake::Exits);
        rig.install("idryx", ".marker-idryx", Fake::Runs);
        let sick = answering("HTTP/1.1 503 Service Unavailable");
        let p = ports(
            rig.healthy("tokenfuse-gateway"),
            rig.healthy("tokenfuse-cloud"),
            silent_port(),
            sick,
        );
        let name = "degraded";

        run_in(rig.args(name, vec![Extra::Wardryx, Extra::Idryx]), &rig.home, p)
            .expect("a failed --with service must not fail up");

        let descriptor = rig.json(&rig.home.descriptor_path(name));
        let services = descriptor["services"].as_object().expect("services map");
        assert_eq!(services.len(), 2, "{descriptor}");
        let wardryx = descriptor["unavailable"]["wardryx"].as_str().unwrap_or("");
        assert!(wardryx.contains("wardryx did not become healthy"), "{descriptor}");
        let idryx = descriptor["unavailable"]["idryx"].as_str().unwrap_or("");
        assert!(idryx.contains("timed out"), "a 503 is not healthy: {descriptor}");
        assert!(descriptor["keys"].get("wardryx_admin_ref").is_none());
        assert!(descriptor["events"]["files"].get("wardryx").is_none());
        let keyfile = rig.json(&rig.home.keyfile_path(name));
        assert!(keyfile["secrets"].get("wardryx_admin").is_none(), "{keyfile}");
        assert_eq!(rig.alive().len(), 2, "only gateway and cloud are left running");

        down(&rig, name).expect("down");
        assert!(rig.alive().is_empty());
    }

    #[test]
    fn optional_services_that_cannot_be_found_or_built_degrade_instead_of_aborting() {
        let rig = Rig::new("nobuild");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        // No cached wardryx: a build is attempted in an empty checkout and fails.
        // No idryx checkout at all: it cannot even be located.
        std::fs::remove_dir_all(rig.workspace.join("idryx")).expect("remove idryx checkout");
        let name = "nobuild";

        run_in(
            rig.args(name, vec![Extra::Wardryx, Extra::Idryx]),
            &rig.home,
            healthy_ports(&rig),
        )
        .expect("up");

        let descriptor = rig.json(&rig.home.descriptor_path(name));
        let wardryx = descriptor["unavailable"]["wardryx"].as_str().unwrap_or("");
        assert!(wardryx.contains("go build"), "{descriptor}");
        let idryx = descriptor["unavailable"]["idryx"].as_str().unwrap_or("");
        assert!(idryx.contains("idryx"), "{descriptor}");
        assert_eq!(descriptor["services"].as_object().map(|m| m.len()), Some(2));
        down(&rig, name).expect("down");
    }

    #[test]
    fn devkey_hands_cloud_the_devkey_and_records_it_as_the_bearer() {
        let rig = Rig::new("devkey");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        let mut args = rig.args("devkey", vec![]);
        args.devkey = true;
        args.gateway_mode = "shadow".to_string();
        args.upstream = Some("https://upstream.invalid/v1/messages".to_string());

        run_in(args, &rig.home, healthy_ports(&rig)).expect("up --devkey");

        let keyfile = rig.json(&rig.home.keyfile_path("devkey"));
        assert_eq!(secret(&keyfile, "cloud_admin"), "devkey");
        assert_eq!(secret(&keyfile, "cloud_viewer"), "devkey");
        let cloud_log = rig.log("devkey", "cloud");
        assert!(cloud_log.contains("TOKENFUSE_CLOUD_ALLOW_DEVKEY=1\n"), "{cloud_log}");
        assert!(cloud_log.contains("TOKENFUSE_CLOUD_KEYS=\n"), "{cloud_log}");
        let gateway_log = rig.log("devkey", "gateway");
        assert!(gateway_log.contains("TOKENFUSE_MODE=shadow\n"));
        assert!(gateway_log.contains("TOKENFUSE_UPSTREAM=https://upstream.invalid/v1/messages\n"));
        assert!(gateway_log.contains("TOKENFUSE_ALLOW_STUB=<unset>\n"));
        let descriptor = rig.json(&rig.home.descriptor_path("devkey"));
        assert_eq!(descriptor["services"]["gateway"]["mode"], "shadow");
        down(&rig, "devkey").expect("down");
    }

    /// Put a directory where `up` wants to write one of its three files, so
    /// that write fails after both services are already running.
    fn up_with_a_blocked_write(label: &str, blocked: &Path) -> (Rig, anyhow::Error) {
        let rig = Rig::new(label);
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        let blocked = rig.home.environments_dir().join(blocked);
        std::fs::create_dir_all(&blocked).expect("block the path with a directory");
        let err = run_in(rig.args(label, vec![]), &rig.home, healthy_ports(&rig))
            .expect_err("a failed write must fail up");
        (rig, err)
    }

    #[test]
    fn a_pidfile_that_cannot_be_written_stops_everything_started() {
        let (rig, err) = up_with_a_blocked_write("nopid", Path::new("nopid.pid.json"));
        assert!(format!("{err:#}").starts_with("write pidfile"), "{err:#}");
        assert!(rig.pid_of("tokenfuse-cloud").is_some(), "cloud did start");
        assert!(rig.alive().is_empty(), "unaccounted processes: {:?}", rig.alive());
        assert!(!rig.home.keyfile_path("nopid").exists());
    }

    #[test]
    fn a_keyfile_that_cannot_be_written_rolls_back_and_removes_the_pidfile() {
        let (rig, err) = up_with_a_blocked_write("nokey", Path::new("nokey.keys.json"));
        assert!(format!("{err:#}").starts_with("write keyfile"), "{err:#}");
        assert!(rig.alive().is_empty(), "unaccounted processes: {:?}", rig.alive());
        assert!(!rig.home.pidfile_path("nokey").exists());
        assert!(!rig.home.descriptor_path("nokey").exists());
    }

    #[test]
    fn a_descriptor_that_cannot_be_written_rolls_back_and_removes_the_secrets() {
        let (rig, err) = up_with_a_blocked_write("nodesc", Path::new("nodesc.json"));
        assert!(format!("{err:#}").starts_with("write descriptor"), "{err:#}");
        assert!(rig.alive().is_empty(), "unaccounted processes: {:?}", rig.alive());
        assert!(!rig.home.pidfile_path("nodesc").exists());
        assert!(
            !rig.home.keyfile_path("nodesc").exists(),
            "live bearer tokens must not be left behind by a failed up"
        );
    }

    #[test]
    fn bad_arguments_are_refused_before_anything_is_created() {
        let rig = Rig::new("badargs");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        let mut mode = rig.args("badargs", vec![]);
        mode.gateway_mode = "audit".to_string();
        let err = run_in(mode, &rig.home, healthy_ports(&rig)).expect_err("unknown mode");
        assert!(format!("{err:#}").contains("--gateway-mode"), "{err:#}");

        let err = validate_args(&rig.args("../escape", vec![])).expect_err("bad name");
        assert!(!format!("{err:#}").is_empty());
        assert!(rig.pid_of("tokenfuse-gateway").is_none());
        assert!(!rig.home.environments_dir().exists(), "nothing may be created");
    }

    #[test]
    fn a_wardryx_that_cannot_be_prepared_is_unavailable_not_fatal() {
        let rig = Rig::new("noprep");
        rig.install_money_plane(Fake::Runs, Fake::Runs);
        // The demo policy is written into the environments directory; a
        // directory squatting on its path makes that write fail.
        std::fs::create_dir_all(rig.home.wardryx_policy_path("noprep")).expect("block policy");

        run_in(rig.args("noprep", vec![Extra::Wardryx]), &rig.home, healthy_ports(&rig))
            .expect("up");

        let descriptor = rig.json(&rig.home.descriptor_path("noprep"));
        let reason = descriptor["unavailable"]["wardryx"].as_str().unwrap_or("");
        assert!(reason.contains("demo wardryx policy"), "{descriptor}");
        assert!(!rig.log("noprep", "gateway").contains("TOKENFUSE_WARDRYX_URL=http"));
        down(&rig, "noprep").expect("down");
    }
}
