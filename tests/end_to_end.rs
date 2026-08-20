//! Invariant 5, the half that needed a built stack: `up` is idempotent and
//! `down` is complete, proved by running them.
//!
//! WHY THIS IS `#[ignore]`
//!
//! It starts real processes and binds the fixed ports 4100 and 8080 on the
//! machine running it. A `cargo test` that did that would fight whatever the
//! operator has open, on every build and every push. `scripts/e2e.sh` runs it
//! explicitly and refuses to start if either port is already held.
//!
//! WHAT IT COST TO WRITE, WHICH IS THE ARGUMENT FOR HAVING IT
//!
//! The first run did not reach a single assertion. `taipan up` had been broken
//! against its own gateway since 2026-07-25, when tokenfuse made its built-in
//! stub opt-IN (4b4b3fd, "gateway: refuse to start rather than invent usage")
//! and started refusing to run with neither `TOKENFUSE_UPSTREAM` nor
//! `TOKENFUSE_ALLOW_STUB` set. taipan set neither. Four weeks, no CI, and no
//! test in this repository had ever run `up`, so nothing said so.
//!
//! HOW IT AVOIDS LEAVING A STACK RUNNING
//!
//! Rust tests have no teardown, and a panic halfway through would leave the
//! money plane up on somebody's laptop. So nothing asserts until `down` has
//! run: the observations are collected first, the environment is always torn
//! down, and only then is the verdict given.

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

const GATEWAY_PORT: u16 = 4100;
const CLOUD_PORT: u16 = 8080;

fn taipan(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_taipan"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run the taipan binary under test")
}

/// True when something answers on the port. Deliberately a connect rather than
/// a process lookup: invariant 2 forbids deriving a target from `ps`/`lsof`,
/// and a test that checked liveness that way would be teaching the mechanism
/// this repository refuses to use.
fn port_answers(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("loopback addr"),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// True when the port can be bound, which is the strong direction: nothing is
/// listening, as opposed to something listening and refusing us.
fn port_is_free(port: u16) -> bool {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => {
            drop(l);
            true
        }
        Err(e) if e.kind() == ErrorKind::AddrInUse => false,
        // Anything else is not evidence either way, and this test must not
        // report a clean teardown on a question it could not ask.
        Err(e) => panic!("could not test port {port}: {e}"),
    }
}

fn taipan_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME")).join(".taipan")
}

fn env_file(name: &str, suffix: &str) -> PathBuf {
    taipan_home()
        .join("environments")
        .join(format!("{name}{suffix}"))
}

#[test]
#[ignore = "starts real processes and binds ports 4100/8080; run via scripts/e2e.sh"]
fn up_is_idempotent_and_down_leaves_nothing_behind() {
    // A name nothing else will pick, so a run cannot collide with an
    // environment the operator has open.
    let name = format!("e2e-{}", std::process::id());

    assert!(
        port_is_free(GATEWAY_PORT) && port_is_free(CLOUD_PORT),
        "ports {GATEWAY_PORT}/{CLOUD_PORT} must be free before this starts; \
         something else is holding them"
    );

    // --- observations, all taken before anything is asserted ---------------
    let first_up = taipan(&["up", "--name", &name, "--healthz-timeout-secs", "60"]);
    let gateway_answers_after_up = port_answers(GATEWAY_PORT);
    let cloud_answers_after_up = port_answers(CLOUD_PORT);
    let pidfile_after_first = std::fs::read(env_file(&name, ".pid.json")).ok();
    let descriptor_exists = env_file(&name, ".json").is_file();

    // The second `up`. Invariant 5 says it must not start a second copy or
    // corrupt the pidfile; it holds that by refusing outright.
    let second_up = taipan(&["up", "--name", &name, "--healthz-timeout-secs", "10"]);
    let pidfile_after_second = std::fs::read(env_file(&name, ".pid.json")).ok();

    let down = taipan(&["down", "--name", &name]);
    let gateway_free_after_down = port_is_free(GATEWAY_PORT);
    let cloud_free_after_down = port_is_free(CLOUD_PORT);
    let pidfile_after_down = env_file(&name, ".pid.json").exists();
    let keyfile_after_down = env_file(&name, ".keys.json").exists();
    let descriptor_after_down = env_file(&name, ".json").exists();

    // Idempotent in the other direction too.
    let second_down = taipan(&["down", "--name", &name]);

    // Works twice, from empty: the environment must come back up after a
    // clean teardown, not only the first time on a fresh machine.
    let third_up = taipan(&["up", "--name", &name, "--healthz-timeout-secs", "60"]);
    let gateway_answers_again = port_answers(GATEWAY_PORT);

    // --- always tear down, whatever the observations were ------------------
    let final_down = taipan(&["down", "--name", &name]);
    let gateway_free_at_end = port_is_free(GATEWAY_PORT);
    let cloud_free_at_end = port_is_free(CLOUD_PORT);

    // --- and only now, the verdict -----------------------------------------
    let text = |o: &Output| {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };

    assert!(
        first_up.status.success(),
        "the first `up` must succeed.\n{}",
        text(&first_up)
    );
    assert!(
        gateway_answers_after_up,
        "the gateway must answer on {GATEWAY_PORT} after `up`.\n{}",
        text(&first_up)
    );
    assert!(
        cloud_answers_after_up,
        "cloud must answer on {CLOUD_PORT} after `up`.\n{}",
        text(&first_up)
    );
    assert!(pidfile_after_first.is_some(), "`up` must write a pidfile");
    assert!(descriptor_exists, "`up` must write the descriptor");

    assert!(
        !second_up.status.success(),
        "a second `up` on a live environment must refuse, not start a second copy.\n{}",
        text(&second_up)
    );
    assert!(
        String::from_utf8_lossy(&second_up.stderr).contains("already appears to be up"),
        "the refusal must say why and how to recover.\n{}",
        text(&second_up)
    );
    assert_eq!(
        pidfile_after_first, pidfile_after_second,
        "a refused `up` must leave the pidfile byte for byte as it was"
    );

    assert!(
        down.status.success(),
        "`down` must succeed.\n{}",
        text(&down)
    );
    assert!(
        gateway_free_after_down,
        "nothing may hold {GATEWAY_PORT} after `down`.\n{}",
        text(&down)
    );
    assert!(
        cloud_free_after_down,
        "nothing may hold {CLOUD_PORT} after `down`.\n{}",
        text(&down)
    );
    assert!(!pidfile_after_down, "`down` must remove the pidfile");
    assert!(!keyfile_after_down, "`down` must remove the keyfile");
    assert!(!descriptor_after_down, "`down` must remove the descriptor");

    assert!(
        second_down.status.success(),
        "`down` on an environment that is already down is a no-op, not an error.\n{}",
        text(&second_down)
    );
    assert!(
        String::from_utf8_lossy(&second_down.stdout).contains("nothing to stop"),
        "and it must say so plainly.\n{}",
        text(&second_down)
    );

    assert!(
        third_up.status.success(),
        "the environment must come back up after a clean `down`.\n{}",
        text(&third_up)
    );
    assert!(
        gateway_answers_again,
        "and the gateway must answer again.\n{}",
        text(&third_up)
    );

    assert!(
        final_down.status.success() && gateway_free_at_end && cloud_free_at_end,
        "the test must leave nothing running.\n{}",
        text(&final_down)
    );
}

#[test]
#[ignore = "starts real processes and binds ports 4100/8080; run via scripts/e2e.sh"]
fn a_stale_pidfile_does_not_block_a_fresh_up() {
    // The other half of `refuse_if_already_up`: a pidfile whose processes are
    // all gone is stale, not a live environment, and must be overwritten. Get
    // this wrong and a machine that was rebooted while a stack was up can
    // never bring that environment back without deleting a file by hand.
    let name = format!("e2e-stale-{}", std::process::id());
    let pidfile = env_file(&name, ".pid.json");

    assert!(
        port_is_free(GATEWAY_PORT) && port_is_free(CLOUD_PORT),
        "ports {GATEWAY_PORT}/{CLOUD_PORT} must be free before this starts"
    );

    std::fs::create_dir_all(pidfile.parent().expect("environments dir"))
        .expect("create environments dir");
    // PID 2^31-1 is not a live process on any machine this runs on, and it is
    // reached through the same code path a real dead PID would be.
    std::fs::write(
        &pidfile,
        format!(
            r#"{{"name":"{name}","processes":[{{"service":"gateway","pid":2147483647,"pgid":2147483647,"stop_signal":"SIGINT","started_at":"2026-01-01T00:00:00Z"}}]}}"#
        ),
    )
    .expect("write a stale pidfile");

    let up = taipan(&["up", "--name", &name, "--healthz-timeout-secs", "60"]);
    let answered = port_answers(GATEWAY_PORT);

    let down = taipan(&["down", "--name", &name]);
    let free_at_end = port_is_free(GATEWAY_PORT);
    let _ = std::fs::remove_file(&pidfile);

    assert!(
        up.status.success(),
        "a stale pidfile must be overwritten, not treated as a live environment.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(answered, "and the gateway must actually come up");
    assert!(
        down.status.success() && free_at_end,
        "the test must leave nothing running"
    );
}

const WARDRYX_PORT: u16 = 8090;
const IDRYX_PORT: u16 = 8081;

/// The `--with` half, which CLAUDE.md named as the uncovered path and which is
/// the one an operator uses to see anything decide.
///
/// Without it `taipan up` brings up the money plane and nothing governs it.
/// With it, Wardryx is seeded with a policy and the gateway is wired to consult
/// it, and Idryx is remapped off Cloud's port. Three services' worth of build,
/// start and descriptor code had no test at all: `services/idryx.rs` was at 0%
/// and `services/wardryx.rs` at 44%.
///
/// Same rule as the test above: nothing asserts until `down` has run.
#[test]
#[ignore = "starts four services and binds 4100/8080/8090/8081; run via scripts/e2e.sh"]
fn the_optional_planes_come_up_are_described_and_go_down_with_the_rest() {
    let name = format!("e2e-with-{}", std::process::id());

    for p in [GATEWAY_PORT, CLOUD_PORT, WARDRYX_PORT, IDRYX_PORT] {
        assert!(port_is_free(p), "port {p} must be free before this starts");
    }

    // --- observations first ------------------------------------------------
    let up = taipan(&[
        "up",
        "--name",
        &name,
        "--with",
        "wardryx,idryx",
        "--healthz-timeout-secs",
        "90",
    ]);
    let answered: Vec<(u16, bool)> = [GATEWAY_PORT, CLOUD_PORT, WARDRYX_PORT, IDRYX_PORT]
        .iter()
        .map(|&p| (p, port_answers(p)))
        .collect();

    let descriptor = std::fs::read_to_string(env_file(&name, ".json")).ok();
    let policy_seeded = env_file(&name, ".wardryx-policy.yaml").is_file();
    let policy_body = std::fs::read_to_string(env_file(&name, ".wardryx-policy.yaml")).ok();

    // `taipan demo` seeds a synthetic event stream, which is what an operator
    // runs before any real traffic exists. It was at 0%.
    let demo = taipan(&["demo", "--name", &name]);
    let demo_events = taipan_home().join("events").join("demo.ndjson");
    let demo_wrote = std::fs::metadata(&demo_events)
        .map(|m| m.len())
        .unwrap_or(0);

    let down = taipan(&["down", "--name", &name]);
    let free_after: Vec<(u16, bool)> = [GATEWAY_PORT, CLOUD_PORT, WARDRYX_PORT, IDRYX_PORT]
        .iter()
        .map(|&p| (p, port_is_free(p)))
        .collect();

    // --- verdict ------------------------------------------------------------
    let text = |o: &Output| {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };

    assert!(
        up.status.success(),
        "`up --with wardryx,idryx` must succeed.\n{}",
        text(&up)
    );
    for (port, ok) in &answered {
        assert!(
            *ok,
            "port {port} must answer after `up --with`.\n{}",
            text(&up)
        );
    }

    let descriptor = descriptor.expect("`up` must write a descriptor");
    for service in ["gateway", "cloud", "wardryx", "idryx"] {
        assert!(
            descriptor.contains(&format!("\"{service}\"")),
            "the descriptor is what the console auto-discovers, and it must name \
             {service}: {descriptor}"
        );
    }
    // Idryx's own default is 8080, which is Cloud's. The remap is the whole
    // reason it can run beside Cloud at all, and a descriptor carrying the
    // wrong port sends a console to the wrong service rather than to nothing.
    assert!(
        descriptor.contains(&format!("127.0.0.1:{IDRYX_PORT}")),
        "the descriptor must carry Idryx's remapped port: {descriptor}"
    );

    assert!(policy_seeded, "`--with wardryx` must seed the demo policy");
    let policy = policy_body.expect("read the seeded policy");
    assert!(
        policy.contains("agent://mockryx.local/*"),
        "the seeded policy must stay scoped to the rehearsal identities, so it \
         never governs an operator's own agents: {policy}"
    );

    assert!(
        demo.status.success(),
        "`taipan demo` must succeed against a running environment.\n{}",
        text(&demo)
    );
    assert!(
        demo_wrote > 0,
        "`taipan demo` must actually write events to {}",
        demo_events.display()
    );

    assert!(
        down.status.success(),
        "`down` must stop all four.\n{}",
        text(&down)
    );
    for (port, free) in &free_after {
        assert!(
            *free,
            "nothing may hold {port} after `down`.\n{}",
            text(&down)
        );
    }
}
