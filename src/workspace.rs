//! Sibling-repo discovery. `taipan` is designed to sit as a checkout next to
//! the other TAIPANBOX repos (`tokenfuse`, `Idryx`, `wardryx`, ...) under one
//! parent directory, exactly like `bank-in-a-box/run.sh` resolves
//! `$SCRIPT_DIR/../tokenfuse`. Since taipan is a compiled binary rather than
//! a script anchored to its own checkout, the equivalent anchor is the
//! current working directory: run `taipan up` from inside the taipan
//! checkout (workspace = that dir, siblings = its parent's children) or from
//! the shared parent directory itself (workspace = that dir, siblings = its
//! own children), both are tried. `--workspace <dir>` overrides either
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique, empty directory for one test. No `tempfile` crate: invariant 3
    /// keeps the dependency set at the seven declared crates, and the rest of
    /// this repo's tests already build temp paths this way.
    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("taipan-workspace-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn finds_a_sibling_directly_under_the_workspace_root() {
        let root = scratch("under-root");
        std::fs::create_dir_all(root.join("tokenfuse")).expect("create sibling");

        let found = Workspace::new(root.clone())
            .find_repo("tokenfuse", &["tokenfuse"])
            .expect("sibling under the root should be found");

        assert_eq!(found, root.join("tokenfuse"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_a_sibling_under_the_workspace_parent() {
        // The README's ordinary case: taipan is run from inside its own
        // checkout, so the siblings are one level up.
        let base = scratch("under-parent");
        let root = base.join("taipan");
        std::fs::create_dir_all(&root).expect("create workspace root");
        std::fs::create_dir_all(base.join("tokenfuse")).expect("create sibling");

        let found = Workspace::new(root)
            .find_repo("tokenfuse", &["tokenfuse"])
            .expect("sibling under the parent should be found");

        assert_eq!(found, base.join("tokenfuse"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_workspace_root_wins_over_the_parent() {
        // A checkout placed beside taipan overrides one a level up. The order
        // is a decision, so it gets an assertion rather than a comment.
        let base = scratch("root-wins");
        let root = base.join("taipan");
        std::fs::create_dir_all(root.join("tokenfuse")).expect("create inner sibling");
        std::fs::create_dir_all(base.join("tokenfuse")).expect("create outer sibling");

        let found = Workspace::new(root.clone())
            .find_repo("tokenfuse", &["tokenfuse"])
            .expect("a sibling exists in both places");

        assert_eq!(found, root.join("tokenfuse"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn case_variants_are_tried_in_the_order_given() {
        // Idryx is capitalised on disk and lowercase in its module path, which
        // is the entire reason `candidates` is a list.
        //
        // READ THIS BEFORE TRUSTING THIS TEST ON A MAC. The default macOS
        // filesystem is case INSENSITIVE, so `root.join("wardryx").is_dir()`
        // is already true when only `Wardryx` exists on disk, and `find_repo`
        // then returns the string the caller built rather than the name on
        // disk. Two consequences, both real:
        //
        //   1. asserting the returned path equals `root.join("Wardryx")` fails
        //      here and passes on Linux, which is why this compares resolved
        //      paths instead of strings;
        //   2. on a case-insensitive filesystem this scenario CANNOT go red.
        //      Break the loop so it only ever tries the first candidate and
        //      this still passes on a Mac, because the first candidate
        //      resolves. It is a genuine test of the candidate list only on a
        //      case-sensitive filesystem.
        //
        // Kept rather than deleted: it is the only case-variant coverage there
        // is, it is honest about its own limit, and it does hold on Linux.
        let root = scratch("case-variants");
        let on_disk = root.join("Wardryx");
        std::fs::create_dir_all(&on_disk).expect("create sibling");

        let found = Workspace::new(root.clone())
            .find_repo("wardryx", &["wardryx", "Wardryx"])
            .expect("the second candidate should be tried");

        assert!(found.is_dir(), "found path must be a directory: {found:?}");
        assert_eq!(
            std::fs::canonicalize(&found).expect("canonicalize found"),
            std::fs::canonicalize(&on_disk).expect("canonicalize on-disk"),
            "the candidate that resolves must be the directory that exists"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_named_like_the_repo_is_not_a_checkout() {
        let root = scratch("file-not-dir");
        std::fs::write(root.join("tokenfuse"), b"not a checkout").expect("create file");

        let err = Workspace::new(root.clone())
            .find_repo("tokenfuse", &["tokenfuse"])
            .expect_err("a file is not a checkout");

        assert!(
            err.to_string().contains("not found"),
            "a file with the right name must not be accepted, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn not_found_names_every_path_tried_and_points_at_the_flag() {
        let base = scratch("not-found");
        let root = base.join("taipan");
        std::fs::create_dir_all(&root).expect("create workspace root");

        let err = Workspace::new(root.clone())
            .find_repo("tokenfuse", &["tokenfuse", "TokenFuse"])
            .expect_err("nothing to find");
        let msg = err.to_string();

        // Both bases and both candidates, so an operator can see exactly where
        // it looked rather than guessing at two directories.
        for expected in [
            root.join("tokenfuse"),
            root.join("TokenFuse"),
            base.join("tokenfuse"),
            base.join("TokenFuse"),
        ] {
            assert!(
                msg.contains(&expected.display().to_string()),
                "error should name {}, got: {msg}",
                expected.display()
            );
        }
        assert!(
            msg.contains("--workspace"),
            "error should tell the operator about --workspace, got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
