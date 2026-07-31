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

    /// Write as pretty JSON, mode 0600, best-effort permission tightening
    /// (a failure to chmod is logged by the caller, not fatal: the file is
    /// still written, and worst case is a too-loose local dev secret, not a
    /// half-written environment).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize keyfile")?;
        std::fs::write(path, body).with_context(|| format!("write {}", path.display()))?;

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
