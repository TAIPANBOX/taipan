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

    /// Write as pretty JSON, created at mode 0600 from the first byte.
    ///
    /// This file carries live Cloud/Wardryx bearer tokens, so there must be
    /// no window where it exists at a looser mode. Writing at the umask
    /// default and chmod-ing afterward (the previous approach) leaves such a
    /// window open, and leaves the file at that loose mode forever if the
    /// chmod call itself fails: `save` would return `Err`, but the tokens
    /// would already be on disk, readable by anyone the umask allowed. Using
    /// `OpenOptions` with an explicit `mode` asks the OS to apply the mode
    /// atomically as part of creating the file, closing the window rather
    /// than narrowing it afterward. The `set_permissions` call below stays as
    /// belt and braces for the one case creation-mode cannot cover: an
    /// existing file at that path from a previous run, whose mode a bare
    /// `create(true)` does not change (the mode argument to `open(2)` only
    /// applies when a new inode is actually created).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize keyfile")?;

        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
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
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

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
                loop {
                    if let Ok(meta) = std::fs::metadata(&poll_path) {
                        let mode = meta.permissions().mode() & 0o777;
                        if mode != 0o600 {
                            seen.store(mode, Ordering::SeqCst);
                        }
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

            poller.join().expect("poller thread must not panic");
            result.expect("save must succeed on a fresh writable path");
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
