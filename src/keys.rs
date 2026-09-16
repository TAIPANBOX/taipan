//! Dev-convenience bearer keys for Cloud and Wardryx (`key:org[:role]`, per
//! `tokenfuse/crates/cloud/src/keys.rs` and `wardryx/internal/api/auth.go`).
//!
//! The descriptor (07 §7) says key *values* belong in a secret store and only
//! a *reference* belongs in the file consumers auto-discover. Genaryx's own
//! Keychain-backed connector is future work, so for v0 the real secrets go in
//! a sibling `<name>.keys.json` file (mode 0600, never the descriptor) and
//! the descriptor carries only a lookup label (`taipan/<name>/<label>`)
//! pointing at that file's own key names.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::{now_rfc3339, random_hex};

pub struct DevKey {
    /// The bare bearer token a client sends (`tp_<hex>`), and the value that
    /// belongs in the keyfile secret. The server (tokenfuse `parse_keys`,
    /// wardryx `auth`) indexes its key map by the bare token before the first
    /// colon and does an exact lookup on the raw bearer, so a `token:org:role`
    /// bearer never matches a key stored as `token`. Writing the full spec as
    /// the keyfile secret is what broke console auto-discovery auth (both reads
    /// and pairing 401'd, verified live); the secret must be the bare token.
    pub token: String,
    /// The full `token:org:role` entry for the server's `TOKENFUSE_CLOUD_KEYS`
    /// / wardryx keys env. The server parses this into `token -> Principal{org,
    /// role}`; a client never sends this form.
    pub config_spec: String,
}

/// Mint one dev key: a bare `tp_<hex>` token plus its `token:org:role` config
/// spec. 20 random bytes (40 hex chars) is ample entropy for a local/dev
/// bearer; this is explicitly not a production credential (see module docs).
pub fn generate(org: &str, role: &str) -> Result<DevKey> {
    let token = format!("tp_{}", random_hex(20)?);
    let config_spec = format!("{token}:{org}:{role}");
    Ok(DevKey { token, config_spec })
}

/// The descriptor-facing reference label for a given environment + secret
/// name (e.g. `cloud_admin`, `wardryx_viewer`). Must match the key name used
/// in the corresponding `<name>.keys.json` `secrets` map.
pub fn key_ref(env_name: &str, label: &str) -> String {
    format!("taipan/{env_name}/{label}")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KeyFile {
    pub name: String,
    pub created_at: String,
    /// label (e.g. "cloud_admin") -> full bearer spec ("tp_...:org:admin").
    pub secrets: BTreeMap<String, String>,
}

impl KeyFile {
    pub fn new(name: &str, secrets: BTreeMap<String, String>) -> Self {
        Self {
            name: name.to_string(),
            created_at: now_rfc3339(),
            secrets,
        }
    }

    /// Write as pretty JSON, created at mode 0600 from the first byte, over
    /// an always-fresh inode.
    ///
    /// This file carries live Cloud/Wardryx bearer tokens, so there must be
    /// no window where it exists at a looser mode, and the path must never
    /// be followed if something other than a plain file already sits there.
    ///
    /// An earlier version opened with `create(true).truncate(true)`, which
    /// per open(2) reuses an existing inode and ignores the `mode` argument
    /// for it (the mode only applies when a new inode is actually created):
    /// an existing file from a previous run kept its old mode while the new
    /// tokens were written into it, and only the trailing `set_permissions`
    /// call narrowed it afterward, the same write-loose-then-chmod window
    /// this module exists to close, just reachable through an existing file
    /// rather than a fresh one (Fable review, finding 1). The same open call
    /// also followed a symlink at the path like any ordinary path, writing
    /// the tokens into whatever it pointed at (finding 2).
    ///
    /// So before creating, anything at the path that is a plain file (not a
    /// symlink) is removed first, and creation itself uses `create_new`
    /// (`O_CREAT | O_EXCL`), which always makes a brand new inode and never
    /// opens through an existing directory entry. A symlink at the path is
    /// deliberately left untouched: `create_new` then reports `AlreadyExists`
    /// for it without following it, so a symlink is refused rather than
    /// silently deleted or written through. The trailing `set_permissions`
    /// call stays as belt and braces for the one thing creation-mode alone
    /// cannot guarantee: a caller umask that masks bits out of 0600 itself
    /// (an unusual umask, but `open(2)`'s mode argument is masked by it same
    /// as any other creation).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize keyfile")?;

        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                // Leave it: `create_new` below reports AlreadyExists for a
                // symlink without following it, which is the refusal we want.
            }
            Ok(_) => {
                // A plain file (or other non-symlink entry) from a previous
                // run: remove it so creation below always gets a fresh inode.
                std::fs::remove_file(path)
                    .with_context(|| format!("remove stale {}", path.display()))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("stat {}", path.display()));
            }
        }

        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("create {} at mode 0600", path.display()))?;
        file.write_all(body.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
        drop(file);

        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("chmod 600 {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    /// `libc::umask` is process-global, and cargo test runs tests in parallel
    /// by default. Any test that changes the process umask, even briefly,
    /// must hold this for the whole change-save-restore span so it cannot
    /// interleave with another such test and leave the process at umask 0
    /// (Fable finding 6).
    static UMASK_LOCK: Mutex<()> = Mutex::new(());

    /// The keyfile carries live Cloud/Wardryx bearer tokens (module doc
    /// comment above). `save` used to write the file at the umask default and
    /// only afterward chmod it to 0600, so for a moment (or forever, if the
    /// chmod step itself errored) it sat on disk at whatever the caller's
    /// umask allowed. A second reader racing the write, polling as fast as it
    /// can, must never observe any mode but 0600: create-with-mode closes the
    /// window rather than narrowing it after the fact. One iteration is not
    /// reliable (the window is a couple of syscalls wide), so this sweeps
    /// many, the way `durability-sweep.sh` sweeps seeds rather than trying
    /// three values.
    #[test]
    fn save_never_lets_a_second_reader_observe_a_looser_mode() {
        let _umask_guard = UMASK_LOCK.lock().expect("umask lock poisoned");
        const ITERATIONS: usize = 2000;

        let dir = std::env::temp_dir().join(format!(
            "taipan-keys-race-{}-{}",
            std::process::id(),
            random_hex(4).expect("random dir suffix")
        ));
        std::fs::create_dir_all(&dir).expect("create race scratch dir");

        let loosest_mode_seen: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));

        for i in 0..ITERATIONS {
            let path = dir.join(format!("race-{i}.keys.json"));

            let poll_path = path.clone();
            let seen = loosest_mode_seen.clone();
            let poller = std::thread::spawn(move || {
                // Busy-poll for the file to appear and record the mode the
                // instant it does. On the old write-then-chmod code this can
                // catch the umask-default mode before the later chmod call
                // narrows it; on create-with-mode there is nothing looser to
                // catch because the file never exists at any other mode.
                // Bounded (Fable finding 5): if `save` ever errors before the
                // file is created (a failing `create_dir_all` or `serialize`),
                // the file never appears and an unbounded loop here would
                // hang the whole test suite rather than fail it.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    if let Ok(meta) = std::fs::metadata(&poll_path) {
                        let mode = meta.permissions().mode() & 0o777;
                        if mode != 0o600 {
                            seen.store(mode, Ordering::SeqCst);
                        }
                        return;
                    }
                    if std::time::Instant::now() >= deadline {
                        return;
                    }
                }
            });

            // A permissive umask so a write-then-chmod implementation has
            // something loose to be caught at; restored immediately after.
            let old_umask = unsafe { libc::umask(0) };
            let kf = KeyFile::new("race", BTreeMap::new());
            let result = kf.save(&path);
            unsafe { libc::umask(old_umask) };

            // Check the save result BEFORE joining the poller (Fable finding
            // 5): the poller is now bounded above so this ordering no longer
            // changes whether the test can hang, but checking the substantive
            // failure first gives a clearer panic message than a stalled
            // poller would, if `save` ever regresses to erroring here.
            result.expect("save must succeed on a fresh writable path");
            poller.join().expect("poller thread must not panic");
            let _ = std::fs::remove_file(&path);
        }

        let _ = std::fs::remove_dir_all(&dir);

        let seen = loosest_mode_seen.load(Ordering::SeqCst);
        assert_eq!(
            seen, 0,
            "a second reader observed the keyfile at mode {seen:03o} (looser \
             than 0600) before the explicit chmod ran, across {ITERATIONS} tries"
        );
    }

    /// The non-racing half of the same property: once `save` returns
    /// successfully, the file's final mode is 0600 regardless of the
    /// caller's umask. This holds for both the old and the fixed
    /// implementation when nothing fails partway (chmod ordinarily
    /// succeeds); it is the sweep above, not this one, that distinguishes
    /// them, because only the sweep can see the window in between.
    #[test]
    fn save_leaves_the_file_at_mode_0600_under_a_permissive_umask() {
        let _umask_guard = UMASK_LOCK.lock().expect("umask lock poisoned");
        let dir = std::env::temp_dir().join(format!(
            "taipan-keys-umask-{}-{}",
            std::process::id(),
            random_hex(4).expect("random dir suffix")
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join("env.keys.json");

        let old_umask = unsafe { libc::umask(0) };
        let kf = KeyFile::new("umask-check", BTreeMap::new());
        let result = kf.save(&path);
        unsafe { libc::umask(old_umask) };
        result.expect("save");

        let mode = std::fs::metadata(&path)
            .expect("keyfile must exist after save")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "final mode must be 0600, got {mode:03o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fable finding 1: `save` used to open an EXISTING file with
    /// `create(true).truncate(true)`, which per open(2) reuses the old inode
    /// and ignores the `mode` argument (it only applies when a new inode is
    /// actually created). The new tokens were then written into that reused
    /// inode at its OLD mode, and only the trailing `set_permissions` call
    /// narrowed it afterward, exactly the write-loose-then-chmod window this
    /// module's docs say is closed. The final mode alone does not catch this
    /// (the trailing chmod already made it 0600 on the old code too, race
    /// window aside), so this test also asserts the inode changed, which only
    /// happens if the stale file was actually replaced rather than reused.
    ///
    /// The hostile umask (masking every bit) exists so that if the trailing
    /// `set_permissions` belt-and-braces were ever deleted, a freshly
    /// created 0600-mode file would land at 0000 instead, catching that
    /// mutant here rather than only in the fresh-path race sweep above (Fable
    /// mutant M1 on this finding: "delete set_permissions").
    #[test]
    fn save_replaces_an_existing_file_rather_than_reusing_its_inode() {
        let _umask_guard = UMASK_LOCK.lock().expect("umask lock poisoned");
        let dir = std::env::temp_dir().join(format!(
            "taipan-keys-existing-{}-{}",
            std::process::id(),
            random_hex(4).expect("random dir suffix")
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join("env.keys.json");

        std::fs::write(&path, b"stale from a previous run").expect("seed stale file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("chmod seed file to 0644");
        let original_inode = std::fs::metadata(&path).expect("stat seed file").ino();

        let old_umask = unsafe { libc::umask(0o777) };
        let kf = KeyFile::new("existing", BTreeMap::new());
        let result = kf.save(&path);
        unsafe { libc::umask(old_umask) };
        result.expect("save must succeed over an existing file, not refuse it");

        let meta = std::fs::metadata(&path).expect("keyfile must exist after save");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "final mode must be 0600 even over a stale 0644 file, got {mode:03o}"
        );
        assert_ne!(
            meta.ino(),
            original_inode,
            "save must not reuse the existing file's inode; it must replace it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fable finding 2: a symlink at the keyfile path used to be followed,
    /// because `create(true)` without `O_EXCL` opens through a symlink like
    /// an ordinary path. `save` would write the new tokens into whatever the
    /// link pointed at, and on a dangling link would create the target file
    /// in another directory entirely. `save` must refuse a symlink at the
    /// path (`AlreadyExists`), never follow it.
    #[test]
    fn save_refuses_a_symlink_at_the_path_rather_than_following_it() {
        let dir = std::env::temp_dir().join(format!(
            "taipan-keys-symlink-{}-{}",
            std::process::id(),
            random_hex(4).expect("random dir suffix")
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");

        let target = dir.join("elsewhere.json");
        let link = dir.join("env.keys.json");
        std::fs::write(&target, b"do not touch").expect("seed symlink target");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let kf = KeyFile::new("symlinked", BTreeMap::new());
        let err = kf
            .save(&link)
            .expect_err("save must refuse a symlink at the path, not follow it");

        let io_err = err
            .downcast_ref::<std::io::Error>()
            .expect("save's error must wrap an io::Error");
        assert_eq!(
            io_err.kind(),
            std::io::ErrorKind::AlreadyExists,
            "expected AlreadyExists refusing the symlink, got {io_err:?}"
        );

        let target_body = std::fs::read_to_string(&target).expect("read symlink target");
        assert_eq!(
            target_body, "do not touch",
            "save must not have written through the symlink into its target"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink must still be at the path")
                .file_type()
                .is_symlink(),
            "save must leave the symlink itself in place, not delete it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the auto-discovery auth bug: the keyfile secret (what a
    /// client sends as its bearer) must be the BARE token, because the server
    /// indexes its key map by the bare token before the first colon. A secret
    /// carrying the `:org:role` suffix 401s for both reads and pairing.
    #[test]
    fn keyfile_secret_is_the_bare_token_and_config_spec_is_full() {
        let k = generate("acme", "admin").expect("generate");
        assert!(k.token.starts_with("tp_"), "token must be a tp_ bearer");
        assert!(
            !k.token.contains(':'),
            "keyfile secret must be the bare token (no :org:role), got {:?}",
            k.token
        );
        assert_eq!(
            k.config_spec,
            format!("{}:acme:admin", k.token),
            "server config spec keeps token:org:role"
        );
    }
}
