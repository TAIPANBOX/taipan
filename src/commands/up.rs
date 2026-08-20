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
fn prepare_wardryx_wiring(org: &str, home: &TaipanHome, name: &str) -> Result<WardryxWiring> {
    let admin = keys::generate(org, "admin")?;
    let viewer = keys::generate(org, "viewer")?;
    // HMAC key wardryx signs/verifies approval_token with (WARDRYX_APPROVAL_SECRET).
    // 32 random bytes is ample entropy for a local/dev secret, matching the
    // 20-byte dev bearer keys `keys::generate` already mints.
    let approval_secret = random_hex(32)?;
    let policy_path = home.wardryx_policy_path(name);
    services::wardryx::write_demo_policy(&policy_path)?;
    Ok(WardryxWiring {
        url: format!("http://127.0.0.1:{}", services::wardryx::PORT),
        admin,
        viewer,
        approval_secret,
        policy_path,
    })
}

pub fn run(args: UpArgs) -> Result<()> {
    validate_name(&args.name)?;
    if !GATEWAY_MODES.contains(&args.gateway_mode.as_str()) {
        anyhow::bail!(
            "--gateway-mode must be one of shadow|warn|enforce (got {:?})",
            args.gateway_mode
        );
    }
    let healthz_timeout = Duration::from_secs(args.healthz_timeout_secs.max(1));

    let home = TaipanHome::discover()?;
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
        match prepare_wardryx_wiring(&org, &home, &args.name) {
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

    let (gateway_bin, cloud_bin) = services::tokenfuse_build::ensure_binaries(&workspace, &home)
        .context("locate/build tokenfuse gateway + cloud")?;

    let gateway_log = logs_dir.join("gateway.log");
    let gateway_data_dir = home.traces_dir(&args.name, "gateway");
    match services::gateway::start(
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
            &workspace,
            &home,
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
            &workspace,
            &home,
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

    let keyfile = KeyFile::new(&args.name, secrets);
    if let Err(e) = keyfile.save(&home.keyfile_path(&args.name)) {
        tracing::error!(error = %e, "failed to write keyfile; rolling back everything started this run");
        rollback(&started);
        let _ = std::fs::remove_file(&pidfile_path);
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
    if let Err(e) = descriptor.save(&home.descriptor_path(&args.name)) {
        tracing::error!(error = %e, "failed to write descriptor; rolling back everything started this run");
        rollback(&started);
        let _ = std::fs::remove_file(&pidfile_path);
        let _ = std::fs::remove_file(home.keyfile_path(&args.name));
        return Err(e.context("write descriptor"));
    }

    print_summary(&args.name, &descriptor, &home, args.upstream.as_deref());
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
    workspace: &Workspace,
    home: &TaipanHome,
    tokenfuse_events: &std::path::Path,
    logs_dir: &std::path::Path,
    healthz_timeout: Duration,
) -> Result<services::StartedService> {
    let bin = services::idryx::ensure_binary(workspace, home)?;
    let log_path = logs_dir.join("idryx.log");
    services::idryx::start(&bin, tokenfuse_events, &log_path, healthz_timeout)
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
