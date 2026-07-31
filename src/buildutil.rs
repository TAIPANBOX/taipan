//! Build-staleness check, generalized from `bank-in-a-box/run.sh`'s `stale()`:
//! skip a rebuild when the cached binary's marker file is newer than every
//! matching source file in the sibling repo. Hand-rolled recursive walk
//! (rather than pulling in `walkdir`) skipping the handful of directories
//! that are large and never contain source we care about.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// Directories never worth descending into while looking for source changes:
/// build output, VCS metadata, dependency caches, and each repo's own
/// (irrelevant to us) local binary/test-fixture dirs.
const SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "bin",
    "dist",
    ".remember",
    "testdata",
    ".github",
];

/// True when `marker` is missing, or any file under `source_root` with an
/// extension in `extensions` (or a root-relative name in `extra_files`, e.g.
/// `Cargo.lock`) has been modified more recently than `marker`. A missing or
/// unreadable `source_root` is also treated as stale, fail toward rebuilding,
/// never toward silently serving a wrong binary.
pub fn is_stale(
    marker: &Path,
    source_root: &Path,
    extensions: &[&str],
    extra_files: &[&str],
) -> bool {
    let marker_time = match fs::metadata(marker).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };

    for f in extra_files {
        match fs::metadata(source_root.join(f)).and_then(|m| m.modified()) {
            Ok(mt) if mt > marker_time => return true,
            _ => {}
        }
    }

    newer_file_exists(source_root, extensions, marker_time)
}

fn newer_file_exists(dir: &Path, extensions: &[&str], marker_time: SystemTime) -> bool {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return true,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => return true,
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            if newer_file_exists(&path, extensions, marker_time) {
                return true;
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if extensions.contains(&ext) {
                if let Ok(Ok(mt)) = entry.metadata().map(|m| m.modified()) {
                    if mt > marker_time {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn missing_marker_is_stale() {
        let dir = std::env::temp_dir().join(format!("taipan-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(is_stale(&dir.join("nope"), &dir, &["rs"], &[]));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newer_source_file_is_stale_older_is_not() {
        let dir = std::env::temp_dir().join(format!("taipan-test2-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(".marker");
        File::create(&marker).unwrap();
        assert!(!is_stale(&marker, &dir, &["rs"], &[]));

        sleep(Duration::from_millis(20));
        let mut src = File::create(dir.join("main.rs")).unwrap();
        writeln!(src, "fn main() {{}}").unwrap();
        assert!(is_stale(&marker, &dir, &["rs"], &[]));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_target_dir() {
        let dir = std::env::temp_dir().join(format!("taipan-test3-{}", std::process::id()));
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();
        let marker = dir.join(".marker");
        File::create(&marker).unwrap();
        sleep(Duration::from_millis(20));
        File::create(target.join("generated.rs")).unwrap();
        assert!(!is_stale(&marker, &dir, &["rs"], &[]));
        fs::remove_dir_all(&dir).ok();
    }
}
