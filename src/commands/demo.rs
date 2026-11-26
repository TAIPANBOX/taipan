//! `taipan demo`: append a small batch of synthetic agent-event envelopes to
//! the shared events directory, so a fresh environment has something to look
//! at before any real traffic flows. Deliberately simple, a distinct
//! `demo.ndjson` file, clearly labeled synthetic in its own payload, kept
//! separate from the real per-service NDJSON files. The Genaryx console's
//! own `demo` generator (richer, shaped like real campaigns) is a different
//! thing; this is just enough to exercise an ingest path end to end.

use std::fs::OpenOptions;
use std::io::Write as _;

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::DemoArgs;
use crate::home::TaipanHome;
use crate::util::validate_name;

/// (source, type, severity) triples drawn from the event-type registry (07
/// §2), a representative, not exhaustive, spread across services.
const SAMPLE_EVENTS: &[(&str, &str, &str)] = &[
    ("tokenfuse", "budget_exhausted", "critical"),
    ("tokenfuse", "spend_spike", "high"),
    ("tokenfuse", "sustained_loop", "high"),
    ("wardryx", "policy_allow", "info"),
    ("wardryx", "approval_requested", "medium"),
    ("wardryx", "approval_granted", "info"),
    ("engram", "memory_written", "info"),
    ("qryx", "crypto_finding", "medium"),
    ("verdryx", "quality_score", "info"),
    ("mockryx", "sim_run", "info"),
];

pub fn run(args: DemoArgs) -> Result<()> {
    validate_name(&args.name)?;
    let home = TaipanHome::discover()?;
    run_in(&args, &home)
}

/// Everything `demo` does once `~/.taipan` is resolved.
fn run_in(args: &DemoArgs, home: &TaipanHome) -> Result<()> {
    validate_name(&args.name)?;
    let events_dir = home.events_dir();
    std::fs::create_dir_all(&events_dir)
        .with_context(|| format!("create {}", events_dir.display()))?;

    let demo_path = events_dir.join("demo.ndjson");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&demo_path)
        .with_context(|| format!("open {}", demo_path.display()))?;

    // agent_id must match `^agent://[a-z0-9.-]+/[a-z0-9._/-]+$` (07 §1):
    // lowercase the env name and swap '_' for '-' so any valid --name
    // produces a valid agent_id, regardless of validate_name's wider charset.
    let name_segment = args.name.to_lowercase().replace('_', "-");
    let agent_id = format!("agent://taipanbox.dev/demo/{name_segment}");
    let run_id = format!("taipan-demo-{}", chrono::Utc::now().timestamp_millis());
    let base = chrono::Utc::now();

    for i in 0..args.count {
        let (source, event_type, severity) = SAMPLE_EVENTS[i % SAMPLE_EVENTS.len()];
        let ts = base + chrono::Duration::milliseconds((i as i64) * 10);
        let event = json!({
            "schema": "taipanbox.dev/agent-event/v0.2",
            "ts": ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "source": source,
            "type": event_type,
            "agent_id": agent_id,
            "severity": severity,
            "run_id": run_id,
            "data": {
                "seq": i,
                "note": "synthetic event from `taipan demo`, not real telemetry",
            },
        });
        writeln!(file, "{}", serde_json::to_string(&event)?)
            .with_context(|| format!("write to {}", demo_path.display()))?;
    }

    println!(
        "taipan: wrote {} synthetic event(s) to {}",
        args.count,
        demo_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(label: &str) -> (std::path::PathBuf, TaipanHome) {
        let root = std::env::temp_dir().join(format!(
            "taipan-demo-{label}-{}-{}",
            std::process::id(),
            crate::util::random_hex(4).expect("random suffix")
        ));
        let home = TaipanHome {
            root: root.join(".taipan"),
        };
        (root, home)
    }

    fn lines(home: &TaipanHome) -> Vec<serde_json::Value> {
        let body = std::fs::read_to_string(home.events_dir().join("demo.ndjson")).expect("read");
        body.lines()
            .map(|l| serde_json::from_str(l).expect("every line is one JSON event"))
            .collect()
    }

    #[test]
    fn it_appends_count_well_formed_synthetic_events() {
        let (root, home) = home("count");
        let args = DemoArgs {
            name: "My_Env".to_string(),
            count: 12,
        };

        run_in(&args, &home).expect("demo");

        let events = lines(&home);
        assert_eq!(events.len(), 12);
        let run_id = events[0]["run_id"].clone();
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e["schema"], "taipanbox.dev/agent-event/v0.2");
            assert_eq!(e["agent_id"], "agent://taipanbox.dev/demo/my-env");
            assert_eq!(e["data"]["seq"], i);
            assert_eq!(e["run_id"], run_id, "one run id per batch");
            let (source, kind, severity) = SAMPLE_EVENTS[i % SAMPLE_EVENTS.len()];
            assert_eq!(e["source"], source);
            assert_eq!(e["type"], kind);
            assert_eq!(e["severity"], severity);
            assert!(e["data"]["note"]
                .as_str()
                .is_some_and(|n| n.contains("synthetic")));
        }
        assert_eq!(events[10]["source"], events[0]["source"], "the samples cycle");
        let first = events[0]["ts"].as_str().expect("ts");
        let last = events[11]["ts"].as_str().expect("ts");
        assert!(first < last, "timestamps rise within a batch: {first} vs {last}");

        run_in(&args, &home).expect("demo again");
        assert_eq!(lines(&home).len(), 24, "a second batch appends, never truncates");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_bad_name_writes_nothing() {
        let (root, home) = home("bad");
        let args = DemoArgs {
            name: "no/slashes".to_string(),
            count: 3,
        };
        assert!(run_in(&args, &home).is_err());
        assert!(!home.events_dir().join("demo.ndjson").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_events_dir_that_cannot_be_created_is_an_error() {
        let (root, home) = home("blocked");
        std::fs::create_dir_all(&home.root).expect("home root");
        std::fs::write(home.events_dir(), b"a file where the directory goes").expect("block");
        let args = DemoArgs {
            name: "blocked".to_string(),
            count: 1,
        };
        let err = run_in(&args, &home).expect_err("blocked events dir");
        assert!(format!("{err:#}").contains("events"), "{err:#}");
        let _ = std::fs::remove_dir_all(root);
    }
}
