//! Sibling-repo discovery. `taipan` is designed to sit as a checkout next to
//! the other TAIPANBOX repos (`tokenfuse`, `Idryx`, `wardryx`, ...) under one
//! parent directory, exactly like `bank-in-a-box/run.sh` resolves
//! `$SCRIPT_DIR/../tokenfuse`. Since taipan is a compiled binary rather than
//! a script anchored to its own checkout, the equivalent anchor is the
//! current working directory: run `taipan up` from inside the taipan
//! checkout (workspace = that dir, siblings = its parent's children) or from
//! the shared parent directory itself (workspace = that dir, siblings = its
//! own children) — both are tried. `--workspace <dir>` overrides either
//! guess for anyone whose checkouts live elsewhere.
//!
//! No path here is ever hardcoded beyond this documented sibling layout.

use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// `root` should already be an absolute, existing directory (the caller
    /// resolves `--workspace` or the current working directory before
    /// constructing this).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn current_dir() -> Result<Self> {
        let cwd = std::env::current_dir().context("resolve current working directory")?;
        Ok(Self::new(cwd))
    }

    /// Find a sibling repo checkout by trying each of `candidates` (case
    /// variants of the same repo name, e.g. `["Idryx", "idryx"]`) under both
    /// the workspace root and the workspace root's parent.
    pub fn find_repo(&self, repo_label: &str, candidates: &[&str]) -> Result<PathBuf> {
        let mut bases = vec![self.root.clone()];
        if let Some(parent) = self.root.parent() {
            bases.push(parent.to_path_buf());
        }

        let mut tried = Vec::new();
        for base in &bases {
            for name in candidates {
                let p = base.join(name);
                if p.is_dir() {
                    return Ok(p);
                }
                tried.push(p);
            }
        }

        let tried_list = tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "sibling repo '{repo_label}' not found (tried: {tried_list}). \
             taipan expects TAIPANBOX checkouts side by side (see README); \
             pass --workspace <dir> if yours live elsewhere."
        )
    }
}
