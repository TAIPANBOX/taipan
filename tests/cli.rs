//! The binary itself, run the way an operator runs it, against a throwaway
//! `$HOME`. These cover what only the real entry point does: parsing the
//! command line, resolving `~/.taipan` from `$HOME`, and turning an error into
//! a message and a nonzero exit. Nothing here starts a service or binds a
//! port; `up` is only ever driven to a refusal.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch_home(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("taipan-cli-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch home");
    dir
}

fn taipan(home: Option<&PathBuf>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_taipan"));
    cmd.args(args).env_remove("HOME");
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    cmd.output().expect("run the taipan binary under test")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn demo_writes_its_events_under_the_home_it_was_given() {
    let home = scratch_home("demo");
    let out = taipan(Some(&home), &["demo", "--name", "cli", "--count", "3"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let events = home.join(".taipan").join("events").join("demo.ndjson");
    let body = std::fs::read_to_string(&events).expect("demo.ndjson under the given HOME");
    assert_eq!(body.lines().count(), 3);
    assert!(String::from_utf8_lossy(&out.stdout).contains("wrote 3 synthetic event(s)"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn down_on_an_environment_that_does_not_exist_says_so_and_succeeds() {
    let home = scratch_home("down");
    let out = taipan(Some(&home), &["down", "--name", "nothing-here"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("not found (nothing to stop)"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn an_error_is_a_message_and_exit_status_1() {
    let home = scratch_home("errors");
    for args in [
        vec!["up", "--name", "../escape"],
        vec!["up", "--name", "ok", "--gateway-mode", "audit"],
        vec!["down", "--name", "a b"],
        vec!["demo", "--name", ""],
    ] {
        let out = taipan(Some(&home), &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).starts_with("taipan: error: "), "{args:?}: {}", stderr(&out));
    }
    assert!(
        !home.join(".taipan").exists(),
        "a refused command must not create ~/.taipan"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn without_home_it_refuses_rather_than_guessing_a_directory() {
    for args in [
        vec!["demo", "--name", "x"],
        vec!["down", "--name", "x"],
        vec!["up", "--name", "x"],
    ] {
        let out = taipan(None, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(stderr(&out).contains("HOME"), "{args:?}: {}", stderr(&out));
    }
}
