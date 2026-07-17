//! `taipan up`: build/locate the requested services, start them in
//! dependency order (gateway and cloud are mandatory; wardryx/idryx are
//! opt-in via `--with` and degrade gracefully), wait for each one's
//! `/healthz`, then persist the pidfile, keyfile, and descriptor.
//!
//! Fail-closed rules this module enforces:
//! - A failure building/starting/health-checking the mandatory gateway or
//!   cloud aborts the whole command and stops anything already started —
//!   never a half-up stack.
//! - A failure in an opt-in `--with` service does NOT tear down an
//!   already-healthy mandatory pair; it is recorded in the descriptor's
//!   `unavailable` map and logged, so the environment still comes up usable
//!   and nothing is silently faked as green.
//! - A failure persisting the pidfile/keyfile/descriptor itself rolls back
//!   every process started this run: an environment taipan cannot account
//!   for is not left running.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::cli::{Extra, UpArgs};
use crate::descriptor::{Descriptor, EventsSection, KeysSection};
use crate::home::TaipanHome;
use crate::keys::{self, KeyFile};
use crate::pidfile::{PidFile, ProcEntry};
use crate::procutil::{self, Spawned};
use crate::services;
use crate::util::{hostname, now_rfc3339, touch_file, validate_name};
use crate::workspace::Workspace;

const GATEWAY_MODES: [&str; 3] = ["shadow", "warn", "enforce"];

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
    // minted keys. `services::cloud::start` already sets
    // `TOKENFUSE_CLOUD_ALLOW_DEVKEY=1` unconditionally; tokenfuse-cloud's own
    // `parse_keys(spec, allow_devkey)` only inserts the literal `devkey`
    // fallback principal (org=default, role=admin) when the parsed key map
    // is empty, so an empty spec here is what actually flips it on (a
    // non-empty spec would just mint real keys as before, devkey or not).
    // The keyfile then carries the literal string "devkey" under both
    // labels, so a console that auto-discovers this environment reads a
    // bearer Cloud genuinely accepts for pairing, instead of a minted admin
    // key that 401s at `/v1/pair/new` while devkey is the only active
    // credential (issue #20). Dev-only: never the default, only behind this
    // explicit flag.
    let (cloud_keys_spec, cloud_admin_secret, cloud_viewer_secret) = if args.devkey {
        (String::new(), "devkey".to_string(), "devkey".to_string())
    } else {
        let cloud_admin = keys::generate(&org, "admin")?;
        let cloud_viewer = keys::generate(&org, "viewer")?;
        let spec = format!("{},{}", cloud_admin.bearer_spec, cloud_viewer.bearer_spec);
        (spec, cloud_admin.bearer_spec, cloud_viewer.bearer_spec)
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
    match services::cloud::start(&cloud_bin, &cloud_keys_spec, &cloud_log, healthz_timeout) {
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

    if args.with.contains(&Extra::Wardryx) {
        match start_wardryx(
            &workspace,
            &home,
            &org,
            &events_dir,
            &logs_dir,
            healthz_timeout,
        ) {
            Ok((svc, admin_secret, viewer_secret)) => {
                started.push(svc.spawned);
                services_section.insert("wardryx".to_string(), svc.entry);
                event_files.insert("wardryx".to_string(), "wardryx.ndjson".to_string());
                secrets.insert("wardryx_admin".to_string(), admin_secret);
                secrets.insert("wardryx_viewer".to_string(), viewer_secret);
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

    print_summary(&args.name, &descriptor, &home);
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
    org: &str,
    events_dir: &std::path::Path,
    logs_dir: &std::path::Path,
    healthz_timeout: Duration,
) -> Result<(services::StartedService, String, String)> {
    let bin = services::wardryx::ensure_binary(workspace, home)?;
    let events_path = events_dir.join("wardryx.ndjson");
    touch_file(&events_path)?;
    let admin = keys::generate(org, "admin")?;
    let viewer = keys::generate(org, "viewer")?;
    let spec = format!("{},{}", admin.bearer_spec, viewer.bearer_spec);
    let log_path = logs_dir.join("wardryx.log");
    let svc = services::wardryx::start(&bin, &events_path, &spec, &log_path, healthz_timeout)?;
    Ok((svc, admin.bearer_spec, viewer.bearer_spec))
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

fn print_summary(name: &str, descriptor: &Descriptor, home: &TaipanHome) {
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
    println!();
    println!("stop with: taipan down --name {name}");
}
