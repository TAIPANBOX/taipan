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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("taipan-descriptor-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// The money plane only, which is what `taipan up` writes with no `--with`.
    fn money_plane_only() -> Descriptor {
        let mut services = BTreeMap::new();
        services.insert(
            "gateway".to_string(),
            ServiceEntry {
                url: "http://127.0.0.1:4100".to_string(),
                mode: Some("enforce".to_string()),
            },
        );
        services.insert(
            "cloud".to_string(),
            ServiceEntry {
                url: "http://127.0.0.1:8080".to_string(),
                mode: None,
            },
        );
        let mut files = BTreeMap::new();
        files.insert("tokenfuse".to_string(), "tokenfuse.ndjson".to_string());
        Descriptor {
            name: "demo".to_string(),
            created_at: "2026-07-17T10:00:00Z".to_string(),
            host: "my-mac".to_string(),
            services,
            events: EventsSection {
                dir: "/Users/you/.taipan/events".to_string(),
                files,
            },
            keys: KeysSection {
                cloud_admin_ref: Some("taipan/demo/cloud_admin".to_string()),
                cloud_viewer_ref: Some("taipan/demo/cloud_viewer".to_string()),
                ..Default::default()
            },
            unavailable: BTreeMap::new(),
            logs_dir: Some("/Users/you/.taipan/environments/demo.logs".to_string()),
        }
    }

    fn as_json(d: &Descriptor) -> serde_json::Value {
        serde_json::from_str(&serde_json::to_string(d).expect("serialize")).expect("parse back")
    }

    #[test]
    fn the_descriptor_carries_exactly_the_documented_field_names() {
        // Invariant 4: this shape is a contract with Genaryx, and its failure
        // mode is silence. Nothing in THIS repo breaks when a field is renamed;
        // a console in another one simply finds nothing.
        let v = as_json(&money_plane_only());
        for key in ["name", "created_at", "host", "services", "events", "keys"] {
            assert!(v.get(key).is_some(), "descriptor must carry {key:?}: {v}");
        }
        let gateway = &v["services"]["gateway"];
        assert!(gateway.get("url").is_some(), "gateway needs url: {gateway}");
        assert!(
            gateway.get("mode").is_some(),
            "gateway needs mode: {gateway}"
        );
        assert!(
            v["services"]["cloud"].get("url").is_some(),
            "cloud needs url"
        );
        assert!(v["events"].get("dir").is_some(), "events needs dir");
        assert!(v["events"].get("files").is_some(), "events needs files");
    }

    #[test]
    fn an_absent_optional_service_leaves_no_null_behind() {
        // A null is not the same as absent to a consumer that checks presence.
        let v = as_json(&money_plane_only());
        assert!(
            v["services"].get("wardryx").is_none(),
            "no wardryx entry: {v}"
        );
        assert!(v["services"].get("idryx").is_none(), "no idryx entry: {v}");
        assert!(
            v["keys"].get("wardryx_admin_ref").is_none(),
            "an unstarted service must not leave a null key ref: {v}"
        );
        assert!(
            v["services"]["cloud"].get("mode").is_none(),
            "cloud has no mode, and must not carry a null one: {v}"
        );
    }

    #[test]
    fn a_service_that_failed_is_named_in_unavailable_with_a_reason() {
        // The README promises `up` "never omits a failure silently". An
        // omission and a graceful degrade look identical unless the reason is
        // written down.
        let mut d = money_plane_only();
        d.unavailable.insert(
            "wardryx".to_string(),
            "go build for wardryx failed (exit status: 1)".to_string(),
        );
        let v = as_json(&d);
        let reason = v["unavailable"]["wardryx"]
            .as_str()
            .expect("unavailable must name wardryx with a reason");
        assert!(
            reason.contains("wardryx"),
            "the reason must be plain text about the service, got {reason:?}"
        );
    }

    #[test]
    fn an_environment_with_nothing_unavailable_omits_the_section() {
        // An empty object invites a reader to treat "unavailable exists" as
        // "something is unavailable".
        let v = as_json(&money_plane_only());
        assert!(
            v.get("unavailable").is_none(),
            "an empty unavailable map must not be written: {v}"
        );
    }

    #[test]
    fn the_descriptor_round_trips_through_the_file_it_is_written_to() {
        let dir = scratch("roundtrip");
        let path = dir.join("never").join("created").join("demo.json");

        money_plane_only().save(&path).expect("save descriptor");

        assert!(path.is_file(), "save must create its parent directories");
        let read_back: Descriptor =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read descriptor"))
                .expect("a consumer must be able to parse what taipan wrote");
        assert_eq!(as_json(&read_back), as_json(&money_plane_only()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
