//! `taipan down`: stop every process this environment's `up` started, by
//! process group, using only the PIDs recorded in its own pidfile. Never
//! discovers a PID via `ps`/`lsof`/`grep`. Idempotent — running it against an
//! environment that is not up (or already down) is a no-op, not an error.
//!
//! Fail-closed: a process that survives SIGKILL is left in the pidfile (so a
//! retry can find it again) and `down` exits nonzero — it never silently
//! drops a still-alive PID just to report a clean exit.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::cli::DownArgs;
use crate::home::TaipanHome;
use crate::pidfile::PidFile;
use crate::procutil::{stop_group, StopOutcome};
use crate::util::validate_name;

const STOP_GRACE: Duration = Duration::from_secs(10);

pub fn run(args: DownArgs) -> Result<()> {
    validate_name(&args.name)?;
    let home = TaipanHome::discover()?;
    let pidfile_path = home.pidfile_path(&args.name);

    if !pidfile_path.is_file() {
        println!(
            "taipan: environment '{}' not found (nothing to stop)",
            args.name
        );
        return Ok(());
    }

    let pidfile = PidFile::load(&pidfile_path)?;
    let total = pidfile.processes.len();
    let mut still_alive = Vec::new();

    for entry in pidfile.processes.iter().rev() {
        match stop_group(entry.pid, entry.parsed_stop_signal(), STOP_GRACE) {
            Ok(StopOutcome::AlreadyGone) => {
                println!(
                    "  {:<10} pid {:<8} already stopped",
                    entry.service, entry.pid
                );
            }
            Ok(StopOutcome::Stopped) => {
                println!(
                    "  {:<10} pid {:<8} stopped ({})",
                    entry.service, entry.pid, entry.stop_signal
                );
            }
            Ok(StopOutcome::ForceKilled) => {
                println!(
                    "  {:<10} pid {:<8} did not exit gracefully, force-killed",
                    entry.service, entry.pid
                );
            }
            Ok(StopOutcome::StillAlive) => {
                println!(
                    "  {:<10} pid {:<8} STILL ALIVE after SIGKILL; will retry on next `taipan down`",
                    entry.service, entry.pid
                );
                still_alive.push(entry.clone());
            }
            Err(e) => {
                println!(
                    "  {:<10} pid {:<8} error signaling process: {e:#}",
                    entry.service, entry.pid
                );
                still_alive.push(entry.clone());
            }
        }
    }

    if !still_alive.is_empty() {
        still_alive.reverse();
        let remaining = still_alive.len();
        let retry_file = PidFile::new(&args.name, still_alive);
        retry_file
            .save(&pidfile_path)
            .context("write updated pidfile after a partial stop")?;
        anyhow::bail!(
            "taipan: {remaining} of {total} process(es) for '{}' could not be stopped; re-run `taipan down --name {}` to retry",
            args.name,
            args.name
        );
    }

    let _ = std::fs::remove_file(&pidfile_path);
    let _ = std::fs::remove_file(home.keyfile_path(&args.name));
    let _ = std::fs::remove_file(home.descriptor_path(&args.name));

    println!(
        "taipan: environment '{}' stopped and cleaned up ({total} process(es))",
        args.name
    );
    Ok(())
}
