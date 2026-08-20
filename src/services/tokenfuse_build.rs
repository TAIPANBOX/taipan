//! Locate or build the TokenFuse gateway + cloud binaries from the sibling
//! `tokenfuse` repo. One `cargo build` covers both packages (they share a
//! workspace and most of their dependency graph), so this is called once
//! from `commands::up`, ahead of starting either service.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::buildutil::is_stale;
use crate::home::TaipanHome;
use crate::workspace::Workspace;

/// Returns `(gateway_binary, cloud_binary)`, building into
/// `~/.taipan/bin/` when the cached copies are missing or older than any
/// `.rs`/`Cargo.toml`/`Cargo.lock` file in the sibling repo.
pub fn ensure_binaries(workspace: &Workspace, home: &TaipanHome) -> Result<(PathBuf, PathBuf)> {
    let repo = workspace.find_repo("tokenfuse", &["tokenfuse"])?;
    let marker = home.bin_dir().join(".marker-tokenfuse");
    let gateway_bin = home.bin_dir().join("tokenfuse-gateway");
    let cloud_bin = home.bin_dir().join("tokenfuse-cloud");

    let need_build = !gateway_bin.is_file()
        || !cloud_bin.is_file()
        || is_stale(&marker, &repo, &["rs"], &["Cargo.toml", "Cargo.lock"]);

    if !need_build {
        tracing::info!(repo = %repo.display(), "tokenfuse: gateway + cloud up to date, skipping build");
        return Ok((gateway_bin, cloud_bin));
    }

    tracing::info!(repo = %repo.display(), "tokenfuse: building gateway + cloud (release; can take a few minutes on a clean target/)");
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "-p",
            "tokenfuse-gateway",
            "-p",
            "tokenfuse-cloud",
        ])
        .current_dir(&repo)
        .status()
        .context("run `cargo build --release -p tokenfuse-gateway -p tokenfuse-cloud`")?;
    if !status.success() {
        anyhow::bail!("cargo build for tokenfuse-gateway/tokenfuse-cloud failed ({status})");
    }

    std::fs::create_dir_all(home.bin_dir())
        .with_context(|| format!("create directory {}", home.bin_dir().display()))?;
    std::fs::copy(repo.join("target/release/tokenfuse"), &gateway_bin)
        .context("copy built gateway binary (target/release/tokenfuse)")?;
    std::fs::copy(repo.join("target/release/tokenfuse-cloud"), &cloud_bin)
        .context("copy built cloud binary (target/release/tokenfuse-cloud)")?;
    std::fs::write(&marker, b"")
        .with_context(|| format!("write build marker {}", marker.display()))?;

    tracing::info!(
        gateway = %gateway_bin.display(),
        cloud = %cloud_bin.display(),
        "tokenfuse: built"
    );
    Ok((gateway_bin, cloud_bin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("taipan-tfbuild-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// A workspace whose `tokenfuse` sibling exists but is NOT a cargo project.
    /// That is the instrument: `cargo build` inside it cannot succeed, so a run
    /// that returns Ok is a run that never shelled out. A test cannot otherwise
    /// tell a skipped build from a slow one.
    fn workspace_with_an_unbuildable_tokenfuse(label: &str) -> (PathBuf, Workspace) {
        let root = scratch(label);
        std::fs::create_dir_all(root.join("tokenfuse")).expect("create sibling");
        (root.clone(), Workspace::new(root))
    }

    fn home_at(dir: &Path) -> TaipanHome {
        TaipanHome {
            root: dir.to_path_buf(),
        }
    }

    #[test]
    fn a_fresh_cached_build_is_reused_without_shelling_out_to_cargo() {
        let (root, workspace) = workspace_with_an_unbuildable_tokenfuse("reuse");
        let home_dir = root.join("home");
        let home = home_at(&home_dir);
        std::fs::create_dir_all(home.bin_dir()).expect("create bin dir");
        for f in ["tokenfuse-gateway", "tokenfuse-cloud", ".marker-tokenfuse"] {
            std::fs::write(home.bin_dir().join(f), b"").expect("create cached artifact");
        }

        let (gateway, cloud) =
            ensure_binaries(&workspace, &home).expect("a fresh cache must be reused, not rebuilt");

        assert_eq!(gateway, home.bin_dir().join("tokenfuse-gateway"));
        assert_eq!(cloud, home.bin_dir().join("tokenfuse-cloud"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_binary_is_not_treated_as_a_fresh_build() {
        // The marker and the binaries can disagree: a fresh marker beside a
        // deleted binary must still rebuild. Trusting the marker alone hands
        // back a path to a file that is not there, and the failure lands later,
        // at spawn time, wearing a different name.
        let (root, workspace) = workspace_with_an_unbuildable_tokenfuse("missing-bin");
        let home_dir = root.join("home");
        let home = home_at(&home_dir);
        std::fs::create_dir_all(home.bin_dir()).expect("create bin dir");
        for f in ["tokenfuse-gateway", ".marker-tokenfuse"] {
            std::fs::write(home.bin_dir().join(f), b"").expect("create cached artifact");
        }

        let err = ensure_binaries(&workspace, &home)
            .expect_err("a missing cloud binary must not read as up to date");

        // It got as far as trying to build, which is the whole assertion: the
        // early return was not taken.
        assert!(
            err.to_string().contains("cargo build") || err.to_string().contains("tokenfuse-cloud"),
            "expected a build attempt, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
