//! The descriptor `taipan up` writes to `~/.taipan/environments/<name>.json`
//!, the exact artifact the Genaryx console watches for auto-discovery
//! (itrat-console `07-integrations-and-connectors.md` §7, decision D6):
//!
//! ```text
//! {name, created_at, host,
//!  services:{gateway:{url,mode}, cloud:{url}, wardryx:{url}?, idryx:{url}?},
//!  events:{dir, files:{tokenfuse, wardryx, ...}},
//!  keys:{cloud_admin_ref, cloud_viewer_ref, wardryx_admin_ref?, wardryx_viewer_ref?}}
//! ```
//!
//! `unavailable` and `logs_dir` are additive fields beyond that shape (the
//! envelope convention used everywhere else in this stack is
//! `additionalProperties: true`, consumers tolerate fields they don't know
//! about). `unavailable` is how a degraded `--with` service is reported
//! honestly instead of silently omitted.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEntry {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EventsSection {
    pub dir: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeysSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_admin_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_viewer_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wardryx_admin_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wardryx_viewer_ref: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Descriptor {
    pub name: String,
    pub created_at: String,
    pub host: String,
    pub services: BTreeMap<String, ServiceEntry>,
    pub events: EventsSection,
    pub keys: KeysSection,
    /// service name -> human-readable reason it did not come up. Only
    /// populated for services explicitly requested via `--with`; the
    /// mandatory gateway/cloud pair never degrades silently (a failure there
    /// aborts `up` entirely, see commands::up).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unavailable: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs_dir: Option<String>,
}

impl Descriptor {
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize descriptor")?;
        std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
    }
}
