//! Bounded polling of a service's `/healthz` endpoint. Hand-rolled over a raw
//! `TcpStream` rather than pulling in an HTTP client crate: every service in
//! the stack answers `/healthz` with a trivial 2xx and no body worth parsing,
//! so a one-shot HTTP/1.1 request read into a small buffer is all this needs.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::procutil::group_alive;
use crate::util::read_log_tail;

/// One GET to `path` on `127.0.0.1:port`, true if the response's status line
/// starts with `HTTP/1.x 2`.
fn probe_once(port: u16, path: &str, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(_) => return false,
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let text = String::from_utf8_lossy(&buf[..n]);
    text.starts_with("HTTP/1.1 2") || text.starts_with("HTTP/1.0 2")
}

/// Poll `GET http://127.0.0.1:<port><path>` until it answers 2xx, the tracked
/// process dies, or `overall_timeout` elapses, whichever comes first. On
/// failure, the error includes the tail of the process's own log so the
/// operator does not have to go hunting for it.
pub fn wait_healthy(
    port: u16,
    path: &str,
    pid: i32,
    overall_timeout: Duration,
    log_path: &Path,
) -> Result<()> {
    let start = Instant::now();
    let step = Duration::from_millis(200);
    let probe_timeout = Duration::from_millis(500);

    loop {
        if probe_once(port, path, probe_timeout) {
            return Ok(());
        }
        if !group_alive(pid) {
            let tail = read_log_tail(log_path, 40);
            return Err(anyhow::anyhow!(
                "process exited before answering {path} on port {port}\n--- log tail ({}) ---\n{tail}",
                log_path.display()
            ))
            .context("waiting for healthz");
        }
        if start.elapsed() >= overall_timeout {
            let tail = read_log_tail(log_path, 40);
            return Err(anyhow::anyhow!(
                "timed out after {:?} waiting for {path} on port {port}\n--- log tail ({}) ---\n{tail}",
                overall_timeout,
                log_path.display()
            ))
            .context("waiting for healthz");
        }
        std::thread::sleep(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    /// A listener on a free port answering every request with `status`.
    fn answering(status: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let reply = format!("{status}\r\nConnection: close\r\n\r\n");
                let _ = s.write_all(reply.as_bytes());
            }
        });
        port
    }

    /// A live process group, reaped when it ends.
    fn live_group() -> i32 {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null());
        CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        pid
    }

    fn stop(pid: i32) {
        let term = crate::procutil::StopSignal::Term;
        let _ = crate::procutil::stop_group(pid, term, Duration::from_secs(5));
    }

    fn log_with(text: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "taipan-health-{}-{}.log",
            std::process::id(),
            crate::util::random_hex(4).expect("random suffix")
        ));
        std::fs::write(&path, text).expect("write log");
        path
    }

    #[test]
    fn any_2xx_on_http_1_0_or_1_1_is_healthy() {
        let second = Duration::from_secs(1);
        assert!(probe_once(
            answering("HTTP/1.1 204 No Content"),
            "/healthz",
            second
        ));
        assert!(probe_once(answering("HTTP/1.0 200 OK"), "/healthz", second));
    }

    #[test]
    fn a_non_2xx_or_nothing_listening_is_not_healthy() {
        let second = Duration::from_secs(1);
        let unavailable = answering("HTTP/1.1 503 Service Unavailable");
        assert!(!probe_once(unavailable, "/healthz", second));
        assert!(!probe_once(
            answering("HTTP/1.1 302 Found"),
            "/healthz",
            second
        ));
        let free = TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        assert!(!probe_once(free, "/healthz", Duration::from_millis(200)));
    }

    #[test]
    fn a_healthy_answer_returns_at_once() {
        let pid = live_group();
        let log = log_with("");
        let start = Instant::now();
        let port = answering("HTTP/1.1 200 OK");
        wait_healthy(port, "/healthz", pid, Duration::from_secs(5), &log).expect("healthy");
        assert!(start.elapsed() < Duration::from_secs(2));
        stop(pid);
        let _ = std::fs::remove_file(log);
    }

    #[test]
    fn a_timeout_names_the_port_and_carries_the_log_tail() {
        let pid = live_group();
        let log = log_with("line one\nthe service said why\n");
        let port = answering("HTTP/1.1 500 Internal Server Error");
        let err = wait_healthy(port, "/healthz", pid, Duration::from_millis(600), &log)
            .expect_err("a 500 never becomes healthy");
        let text = format!("{err:#}");
        assert!(text.starts_with("waiting for healthz"), "{text}");
        assert!(text.contains("timed out"), "{text}");
        assert!(text.contains(&format!("port {port}")), "{text}");
        assert!(text.contains("the service said why"), "{text}");
        stop(pid);
        let _ = std::fs::remove_file(log);
    }

    #[test]
    fn a_process_that_is_gone_fails_fast_instead_of_waiting_out_the_timeout() {
        let mut cmd = Command::new("true");
        CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().expect("spawn true");
        let pid = child.id() as i32;
        child.wait().expect("reap");
        let log = log_with("crashed on start\n");
        let start = Instant::now();
        let port = answering("HTTP/1.1 503 Nope");
        let err = wait_healthy(port, "/healthz", pid, Duration::from_secs(30), &log)
            .expect_err("a dead process is never healthy");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "it waited out the timeout"
        );
        let text = format!("{err:#}");
        assert!(text.contains("process exited before answering"), "{text}");
        assert!(text.contains("crashed on start"), "{text}");
        let _ = std::fs::remove_file(log);
    }
}
