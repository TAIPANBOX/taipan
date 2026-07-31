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
