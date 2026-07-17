//! Small standalone helpers: environment-name validation, timestamps,
//! hostname, and dev-only random tokens. Kept dependency-light on purpose
//! (this is the open adoption-channel repo, D7) — no `rand`/`hostname`
//! crates, just `libc` (already a dependency for process-group signaling)
//! and a direct `/dev/urandom` read.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// Environment names become path segments (`~/.taipan/environments/<name>.json`)
/// and bearer-key org segments (`taipan-<name>:role`), so they are restricted
/// to a conservative, unambiguous charset up front — fail closed rather than
/// trying to escape/sanitize something that could otherwise be a path
/// traversal (`--name ../../etc`).
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 {
        anyhow::bail!(
            "--name must be 1-63 characters (got {} characters)",
            name.len()
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("--name may only contain ASCII letters, digits, '-' and '_' (got {name:?})");
    }
    Ok(())
}

/// Current time as an RFC 3339 UTC timestamp with second precision, matching
/// the `ts` convention used across the agent-event bus (07 §1).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The local machine's hostname, for the descriptor's `host` field. Falls
/// back to `"localhost"` rather than failing `up` over a cosmetic field.
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is a valid, non-null, correctly-sized buffer for the
    // duration of this one FFI call; `gethostname` writes at most `buf.len()`
    // bytes and null-terminates on success, which is what makes checking for
    // a trailing NUL below well-defined.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if ret != 0 {
        return "localhost".to_string();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    if name.is_empty() {
        "localhost".to_string()
    } else {
        name
    }
}

/// `n_bytes` of cryptographically-random data from `/dev/urandom`, hex
/// encoded. Used only to mint dev-convenience bearer keys (never anything
/// production-security-relevant) — good enough entropy, zero extra
/// dependencies.
pub fn random_hex(n_bytes: usize) -> Result<String> {
    let mut f = File::open("/dev/urandom").context("open /dev/urandom for key generation")?;
    let mut buf = vec![0u8; n_bytes];
    f.read_exact(&mut buf).context("read /dev/urandom")?;
    let mut out = String::with_capacity(n_bytes * 2);
    for b in buf {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}

/// Ensure `path`'s parent directory exists, then create the file if it does
/// not already (leaving any existing content untouched). Idryx's `--load
/// tokenfuse:<path>` requires the file to exist before `idryx serve` starts,
/// and an empty NDJSON file parses as zero events (see `tokenfuse.Load` in
/// Idryx), so touching it up front is enough — there is no ordering
/// requirement between this and starting the gateway that will append to it.
pub fn touch_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("touch {}", path.display()))?;
    Ok(())
}

/// The last `max_lines` lines of a log file, for embedding in error messages
/// when a service fails to become healthy. Never fails — an unreadable log is
/// reported as such rather than aborting the caller's own error path.
pub fn read_log_tail(path: &Path, max_lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            if lines.is_empty() {
                "(log is empty)".to_string()
            } else {
                lines[start..].join("\n")
            }
        }
        Err(e) => format!("(log unavailable: {e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_normal_names() {
        assert!(validate_name("default").is_ok());
        assert!(validate_name("p1smoke").is_ok());
        assert!(validate_name("demo-bank_1").is_ok());
    }

    #[test]
    fn validate_name_rejects_path_traversal_and_empty() {
        assert!(validate_name("").is_err());
        assert!(validate_name("../../etc/passwd").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name(&"x".repeat(64)).is_err());
    }

    #[test]
    fn random_hex_is_hex_and_right_length() {
        let h = random_hex(20).expect("random_hex");
        assert_eq!(h.len(), 40);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hostname_is_nonempty() {
        assert!(!hostname().is_empty());
    }
}
