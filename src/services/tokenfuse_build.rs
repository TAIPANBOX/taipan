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
