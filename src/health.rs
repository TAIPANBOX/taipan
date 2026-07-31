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
