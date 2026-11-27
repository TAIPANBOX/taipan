//! Process spawning and signaling. Every service is launched into its own
//! process group (`setpgid(0, 0)` before exec, via the stable
//! `process_group(0)` builder method) so it can later be stopped as a unit -
//! `taipan down` signals process GROUPS it started itself, by PID captured
//! at spawn time. It never discovers a PID by scanning `ps`/`lsof`/`grep` and
//! never signals a bare PID it did not just fork.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// The signal `taipan down` should try first for a given service, before
/// escalating to SIGKILL. The gateway specifically needs SIGINT, its
/// shutdown future is `tokio::signal::ctrl_c()`, which is what flushes the
/// buffered Parquet trace rows (see bank-in-a-box/run.sh's `stop_gateway`).
/// Every other service accepts a plain SIGTERM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Int,
    Term,
}

impl StopSignal {
    fn as_libc(self) -> libc::c_int {
        match self {
            StopSignal::Int => libc::SIGINT,
            StopSignal::Term => libc::SIGTERM,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StopSignal::Int => "SIGINT",
            StopSignal::Term => "SIGTERM",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "SIGINT" {
            StopSignal::Int
        } else {
            StopSignal::Term
        }
    }
}

/// A process taipan just started: its own process-group leader, so `pid` is
/// also its process-group id.
#[derive(Debug, Clone)]
pub struct Spawned {
    pub service: String,
    pub pid: i32,
    pub stop_signal: StopSignal,
    pub log_path: PathBuf,
}

/// Spawn `program` with `args`/`envs`, stdin closed and stdout+stderr
/// redirected to (truncated, then appended-to) `log_path`, detached into a
/// new process group. Returns immediately after spawn, this does not wait
/// for the process to become ready; callers do that separately (see
/// `crate::health::wait_healthy`) so a not-yet-healthy process is still
/// tracked and can be cleaned up.
#[allow(clippy::too_many_arguments)]
pub fn spawn_process(
    service: &str,
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    cwd: Option<&Path>,
    log_path: &Path,
    stop_signal: StopSignal,
) -> Result<Spawned> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create log directory {}", parent.display()))?;
    }
    let stdout_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
        .with_context(|| format!("open log file {}", log_path.display()))?;
    let stderr_file = stdout_file
        .try_clone()
        .with_context(|| format!("duplicate log file handle for {}", log_path.display()))?;

    let mut cmd = Command::new(program);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(stdout_file));
    cmd.stderr(Stdio::from(stderr_file));
    // New process group, leader = this child (safe stable API: `setpgid(0,
    // 0)` applied between fork and exec). `taipan down` targets -pid (the
    // group), so nothing this process forks internally can be left behind as
    // an orphan outside our reach.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {service} ({})", program.display()))?;
    let pid = child.id() as i32;
    // Deliberately not `child.wait()`-ed: this process must keep running
    // after `taipan up` itself exits. Dropping `Child` here does not kill or
    // signal it (Rust's Child has no such Drop impl), once our own process
    // exits, the child is reparented to init/launchd like any other
    // independent process and reaped normally when it eventually exits.
    drop(child);

    Ok(Spawned {
        service: service.to_string(),
        pid,
        stop_signal,
        log_path: log_path.to_path_buf(),
    })
}

/// Whether the process GROUP led by `pid` still has any living member.
/// Signal 0 performs no actual signaling, only the existence/permission
/// check; targeting the group (negative pid) rather than just the leader
/// catches the case where the leader exited but a forked grandchild lingers.
pub fn group_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 is a pure existence/permission probe; it
    // sends nothing and has no memory-safety preconditions beyond `pid`
    // being a plain integer, which it is.
    let ret = unsafe { libc::kill(-pid, 0) };
    if ret == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Send `sig` to the process group led by `pid`. `ESRCH` (no such
/// process/group) is treated as success, the target is already gone, which
/// is the caller's desired end state, not an error.
fn signal_group(pid: i32, sig: libc::c_int) -> io::Result<()> {
    // SAFETY: same as `group_alive`, `pid` is a plain integer and `sig` is
    // one of the fixed libc signal constants used below.
    let ret = unsafe { libc::kill(-pid, sig) };
    if ret == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The process group was already gone before we signaled it.
    AlreadyGone,
    /// It exited within the grace period after the primary signal.
    Stopped,
    /// It ignored the primary signal but exited after SIGKILL.
    ForceKilled,
    /// It is still alive after SIGKILL and a short wait. Fail-closed: the
    /// caller must report this, not silently drop it.
    StillAlive,
}

/// Stop the process group led by `pid`: try `primary` first, wait up to
/// `grace`, escalate to SIGKILL, wait up to another 3s. Never panics, every
/// outcome, including "still alive", is returned to the caller to report.
pub fn stop_group(pid: i32, primary: StopSignal, grace: Duration) -> Result<StopOutcome> {
    if !group_alive(pid) {
        return Ok(StopOutcome::AlreadyGone);
    }

    signal_group(pid, primary.as_libc())
        .with_context(|| format!("send {} to pid {pid}", primary.as_str()))?;
    if wait_until_gone(pid, grace) {
        return Ok(StopOutcome::Stopped);
    }

    signal_group(pid, libc::SIGKILL).with_context(|| format!("send SIGKILL to pid {pid}"))?;
    if wait_until_gone(pid, Duration::from_secs(3)) {
        return Ok(StopOutcome::ForceKilled);
    }

    Ok(StopOutcome::StillAlive)
}

fn wait_until_gone(pid: i32, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if !group_alive(pid) {
            return true;
        }
        if start.elapsed() >= timeout {
            return !group_alive(pid);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Child;

    /// Spawn a real child in its own process group, exactly the way `spawn`
    /// does, and reap it on a background thread.
    ///
    /// The reaper is not decoration and this test module was wrong without it.
    /// `group_alive` probes with `kill(-pid, 0)`, and an unreaped child that
    /// has already died is a ZOMBIE: the process entry still exists, the probe
    /// returns EPERM rather than ESRCH, and `group_alive` reads EPERM as alive,
    /// deliberately, because "I cannot tell" must fail closed. So a test that
    /// is itself the parent and never reaps will watch a corpse read as living
    /// and then watch `stop_group` escalate to SIGKILL against it.
    ///
    /// Production does not have this problem: `taipan up` exits, its children
    /// are reparented to init, and init reaps them, so by the time a separate
    /// `taipan down` probes the group there is nothing unreaped left. The
    /// reaper thread here is what makes the test model that rather than a
    /// situation the product never meets.
    fn spawn_group_reaped(program: &str, args: &[&str]) -> i32 {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        CommandExt::process_group(&mut cmd, 0);
        let child: Child = cmd.spawn().expect("spawn test child");
        let pid = child.id() as i32;
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
        pid
    }

    #[test]
    fn stop_group_actually_removes_the_group() {
        let pid = spawn_group_reaped("sleep", &["30"]);
        std::thread::sleep(Duration::from_millis(150));
        assert!(group_alive(pid), "a just-spawned group must read as alive");

        let outcome = stop_group(pid, StopSignal::Term, Duration::from_secs(5)).expect("stop");
        assert_eq!(outcome, StopOutcome::Stopped);
        assert!(
            !group_alive(pid),
            "after stop_group reported Stopped the group must actually be gone.              That is the whole claim behind `taipan down` being complete."
        );
    }

    #[test]
    fn stopping_twice_is_a_no_op_not_an_error() {
        let pid = spawn_group_reaped("sleep", &["30"]);
        std::thread::sleep(Duration::from_millis(150));
        stop_group(pid, StopSignal::Term, Duration::from_secs(5)).expect("first stop");

        let again = stop_group(pid, StopSignal::Term, Duration::from_secs(5)).expect("second stop");
        assert_eq!(
            again,
            StopOutcome::AlreadyGone,
            "`taipan down` run twice must be a no-op the second time, not an error"
        );
    }

    #[test]
    fn a_group_that_ignores_the_primary_signal_is_force_killed() {
        // `trap '' TERM` sets SIGTERM to SIG_IGN, and an ignored disposition
        // survives exec, so this is a single `sleep` that cannot be stopped by
        // SIGTERM. Without this case nothing shows the SIGKILL escalation is
        // ever taken, and an escalation path that never runs is a comment.
        let pid = spawn_group_reaped("sh", &["-c", "trap '' TERM; exec sleep 30"]);
        std::thread::sleep(Duration::from_millis(250));
        assert!(group_alive(pid));

        let outcome = stop_group(pid, StopSignal::Term, Duration::from_millis(500)).expect("stop");
        assert_eq!(
            outcome,
            StopOutcome::ForceKilled,
            "a group that ignores the primary signal must be reported as \
             force-killed, never as a clean stop"
        );
        assert!(
            !group_alive(pid),
            "after ForceKilled the group must be gone"
        );
    }
    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "taipan-procutil-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random suffix")
        ))
    }

    /// Reap `pid` on a background thread, so its group really disappears
    /// once it exits (see `spawn_group_reaped`).
    fn reap(pid: i32) {
        std::thread::spawn(move || {
            let mut status: libc::c_int = 0;
            // SAFETY: `pid` is a child this test just spawned and nothing
            // else waits on it; `status` outlives the call.
            unsafe { libc::waitpid(pid, &mut status, 0) };
        });
    }

    #[test]
    fn a_spawned_process_writes_both_streams_to_a_fresh_log_with_its_env_and_cwd() {
        let dir = scratch();
        let log = dir.join("logs").join("svc.log");
        std::fs::create_dir_all(dir.join("logs")).expect("log dir");
        std::fs::write(&log, "left over from an earlier run\n").expect("old log");

        let sp = spawn_process(
            "svc",
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                "echo out=$GREETING; echo err=$(pwd) >&2".to_string(),
            ],
            &[("GREETING".to_string(), "hello".to_string())],
            Some(&dir),
            &log,
            StopSignal::Term,
        )
        .expect("spawn");
        reap(sp.pid);
        assert_eq!(sp.service, "svc");
        assert_eq!(sp.log_path, log);
        assert!(
            wait_until_gone(sp.pid, Duration::from_secs(5)),
            "it ran to completion"
        );

        let text = std::fs::read_to_string(&log).expect("read log");
        assert!(
            !text.contains("left over"),
            "the log is truncated per spawn: {text}"
        );
        assert!(text.contains("out=hello"), "env reached the child: {text}");
        let cwd = std::fs::canonicalize(&dir).expect("canonical dir");
        assert!(
            text.contains(&format!("err={}", cwd.display()))
                || text.contains(&format!("err={}", dir.display())),
            "stderr and the working directory reached the log: {text}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_spawned_process_leads_its_own_group() {
        let dir = scratch();
        let sp = spawn_process(
            "sleeper",
            Path::new("sleep"),
            &["30".to_string()],
            &[],
            None,
            &dir.join("sleeper.log"),
            StopSignal::Int,
        )
        .expect("spawn");
        reap(sp.pid);
        assert!(
            group_alive(sp.pid),
            "the group is named by the child's own pid"
        );
        assert_eq!(
            stop_group(sp.pid, sp.stop_signal, Duration::from_secs(5)).expect("stop"),
            StopOutcome::Stopped
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_program_that_does_not_exist_is_an_error_naming_the_service() {
        let dir = scratch();
        let err = spawn_process(
            "ghost",
            &dir.join("no-such-binary"),
            &[],
            &[],
            None,
            &dir.join("ghost.log"),
            StopSignal::Term,
        )
        .expect_err("nothing to exec");
        assert!(format!("{err:#}").contains("spawn ghost"), "{err:#}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_directory_that_cannot_be_created_is_an_error() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(dir.join("logs"), b"a file in the way").expect("block");
        let err = spawn_process(
            "svc",
            Path::new("true"),
            &[],
            &[],
            None,
            &dir.join("logs").join("svc.log"),
            StopSignal::Term,
        )
        .expect_err("log dir blocked");
        assert!(
            format!("{err:#}").contains("create log directory"),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stop_signals_round_trip_and_anything_unknown_means_sigterm() {
        assert_eq!(StopSignal::parse(StopSignal::Int.as_str()), StopSignal::Int);
        assert_eq!(
            StopSignal::parse(StopSignal::Term.as_str()),
            StopSignal::Term
        );
        assert_eq!(StopSignal::parse("sigint"), StopSignal::Int);
        assert_eq!(StopSignal::parse("SIGHUP"), StopSignal::Term);
        assert_eq!(StopSignal::Int.as_libc(), libc::SIGINT);
        assert_eq!(StopSignal::Term.as_libc(), libc::SIGTERM);
    }
}
