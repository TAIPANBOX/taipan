//! `taipan down`: stop every process this environment's `up` started, by
//! process group, using only the PIDs recorded in its own pidfile. Never
//! discovers a PID via `ps`/`lsof`/`grep`. Idempotent, running it against an
//! environment that is not up (or already down) is a no-op, not an error.
//!
//! Fail-closed: a process that survives SIGKILL is left in the pidfile (so a
//! retry can find it again) and `down` exits nonzero, it never silently
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
    run_in(&args, &home, STOP_GRACE)
}

/// Everything `down` does once `~/.taipan` is resolved. `grace` is how long
/// each group gets after its primary signal before SIGKILL; `run` passes
/// [`STOP_GRACE`].
pub(crate) fn run_in(args: &DownArgs, home: &TaipanHome, grace: Duration) -> Result<()> {
    validate_name(&args.name)?;
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
        match stop_group(entry.pid, entry.parsed_stop_signal(), grace) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pidfile::ProcEntry;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    fn home(label: &str) -> (std::path::PathBuf, TaipanHome) {
        let root = std::env::temp_dir().join(format!(
            "taipan-down-{label}-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random suffix")
        ));
        let home = TaipanHome {
            root: root.join(".taipan"),
        };
        home.ensure_base_dirs().expect("base dirs");
        (root, home)
    }

    fn args(name: &str) -> DownArgs {
        DownArgs {
            name: name.to_string(),
        }
    }

    /// A child in its own process group, the way `spawn_process` starts one.
    fn spawn_group(script: &str) -> Child {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        CommandExt::process_group(&mut cmd, 0);
        cmd.spawn().expect("spawn test child")
    }

    /// The same, reaped on a background thread as soon as it dies, so a
    /// stopped group is really gone (see procutil's lifecycle tests).
    fn spawn_group_reaped(script: &str) -> i32 {
        let mut child = spawn_group(script);
        let pid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        pid
    }

    fn entry(service: &str, pid: i32, signal: &str) -> ProcEntry {
        ProcEntry {
            service: service.to_string(),
            pid,
            pgid: pid,
            stop_signal: signal.to_string(),
            started_at: crate::util::now_rfc3339(),
        }
    }

    fn seed(home: &TaipanHome, name: &str, entries: Vec<ProcEntry>) {
        PidFile::new(name, entries)
            .save(&home.pidfile_path(name))
            .expect("seed pidfile");
        std::fs::write(home.keyfile_path(name), b"{}").expect("seed keyfile");
        std::fs::write(home.descriptor_path(name), b"{}").expect("seed descriptor");
    }

    #[test]
    fn an_environment_that_is_not_up_is_a_no_op() {
        let (root, home) = home("absent");
        run_in(&args("absent"), &home, STOP_GRACE).expect("down on nothing");
        assert!(!home.pidfile_path("absent").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_recorded_group_is_stopped_and_every_file_removed() {
        let (root, home) = home("all");
        let polite = spawn_group_reaped("exec sleep 30");
        let stubborn = spawn_group_reaped("trap '' TERM; exec sleep 30");
        let gone = {
            let mut c = spawn_group("exit 0");
            let pid = c.id() as i32;
            c.wait().expect("reap the short-lived child");
            pid
        };
        std::thread::sleep(Duration::from_millis(250));
        seed(
            &home,
            "all",
            vec![
                entry("gateway", polite, "SIGINT"),
                entry("cloud", stubborn, "SIGTERM"),
                entry("idryx", gone, "SIGTERM"),
            ],
        );

        run_in(&args("all"), &home, Duration::from_millis(500)).expect("down");

        assert!(!crate::procutil::group_alive(polite), "the polite group survived");
        assert!(
            !crate::procutil::group_alive(stubborn),
            "a group ignoring SIGTERM must be force-killed, not left running"
        );
        assert!(!home.pidfile_path("all").exists());
        assert!(!home.keyfile_path("all").exists());
        assert!(!home.descriptor_path("all").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_group_still_there_after_sigkill_is_kept_for_a_retry_and_down_fails() {
        // An unreaped dead child is a zombie: its group still answers the
        // existence probe after SIGKILL, which is exactly what a process
        // `down` cannot get rid of looks like from the outside. `down` must
        // keep it in the pidfile and say so, never drop it to report success.
        let (root, home) = home("stuck");
        let mut zombie = spawn_group("exit 0");
        let stuck = zombie.id() as i32;
        let polite = spawn_group_reaped("exec sleep 30");
        std::thread::sleep(Duration::from_millis(250));
        seed(
            &home,
            "stuck",
            vec![
                entry("gateway", polite, "SIGINT"),
                entry("cloud", stuck, "SIGTERM"),
            ],
        );

        let err = run_in(&args("stuck"), &home, Duration::from_millis(100))
            .expect_err("a group that cannot be stopped must fail down");

        assert!(format!("{err:#}").contains("1 of 2 process(es)"), "{err:#}");
        let left = PidFile::load(&home.pidfile_path("stuck")).expect("pidfile kept");
        let pids: Vec<i32> = left.processes.iter().map(|e| e.pid).collect();
        assert_eq!(pids, [stuck], "only the stuck group is kept for the retry");
        assert!(home.keyfile_path("stuck").exists(), "nothing is cleaned up yet");
        assert!(home.descriptor_path("stuck").exists());
        assert!(!crate::procutil::group_alive(polite));

        zombie.wait().expect("reap the zombie");
        run_in(&args("stuck"), &home, STOP_GRACE).expect("the retry now succeeds");
        assert!(!home.pidfile_path("stuck").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unreadable_pidfile_is_an_error_not_a_silent_success() {
        let (root, home) = home("garbage");
        std::fs::write(home.pidfile_path("garbage"), b"not json").expect("seed");
        let err = run_in(&args("garbage"), &home, STOP_GRACE).expect_err("garbage pidfile");
        assert!(format!("{err:#}").contains("parse"), "{err:#}");
        assert!(home.pidfile_path("garbage").exists(), "the evidence is kept");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_bad_name_is_refused_before_any_file_is_touched() {
        let (root, home) = home("badname");
        assert!(run_in(&args("../badname"), &home, STOP_GRACE).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
