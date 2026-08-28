//! The declaration in `components.json` is only worth reading if this repository
//! proves it, and proves it against the toolchain rather than by describing.
//!
//! estate-gates cannot do this. It has no Rust toolchain, and building
//! twenty-two repositories in its CI is a matrix it does not have. This
//! repository already runs `cargo test` on every push.
//!
//! What is proved here is exactly the `checked` bucket and nothing else. The
//! `declared` bucket is not asserted against anything, on purpose: a test that
//! pretended to verify a sentence about purpose would be the failure this whole
//! design exists to avoid.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> Value {
    let path = root().join("components.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("components.json is valid JSON")
}

fn components(m: &Value) -> Vec<&Value> {
    let cs = m["components"].as_array().expect("components is an array");
    assert!(
        !cs.is_empty(),
        "components.json declares nothing, so every test here measured nothing"
    );
    cs.iter().collect()
}

/// THE ONE THAT CLOSES THE HOLE. A binary this repository builds and does not
/// declare is invisible from outside by construction.
#[test]
fn every_binary_this_workspace_builds_is_declared_and_the_reverse() {
    let m = manifest();
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(root().join("Cargo.toml"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON");

    let mut built: BTreeMap<String, String> = BTreeMap::new();
    for p in meta["packages"].as_array().expect("packages") {
        for t in p["targets"].as_array().expect("targets") {
            if t["kind"]
                .as_array()
                .expect("kind")
                .iter()
                .any(|k| k == "bin")
            {
                built.insert(
                    t["name"].as_str().expect("target name").to_string(),
                    p["name"].as_str().expect("package name").to_string(),
                );
            }
        }
    }
    assert!(
        !built.is_empty(),
        "cargo metadata found no binary, so this measured nothing"
    );

    let declared: BTreeMap<String, String> = components(&m)
        .iter()
        .filter_map(|c| {
            Some((
                c["checked"]["binary"].as_str()?.to_string(),
                c["checked"]["crate"].as_str()?.to_string(),
            ))
        })
        .collect();
    assert!(
        !declared.is_empty(),
        "no component declares a binary, so this measured nothing"
    );

    for b in built.keys() {
        assert!(
            declared.contains_key(b),
            "this workspace builds `{b}` and components.json does not declare it"
        );
    }
    for (b, k) in &declared {
        assert_eq!(
            built.get(b),
            Some(k),
            "components.json says `{b}` comes from crate `{k}`; cargo says {:?}",
            built.get(b)
        );
    }
}

/// THE ONE NO OTHER MANIFEST IN THIS ESTATE CAN MAKE.
///
/// Every other repository answers "what do I build". This one is a launcher, so
/// it answers "what do I bring up", and that is checked against `src/services/`,
/// the modules that do the bringing up.
///
/// The mapping is deliberate rather than clever: a module is named after the
/// process it supervises, and `tokenfuse_build` is not one of them because it
/// locates or builds binaries rather than running any.
#[test]
fn everything_it_says_it_installs_has_a_module_that_installs_it() {
    let m = manifest();

    let mut declared: BTreeSet<String> = BTreeSet::new();
    for c in components(&m) {
        if let Some(list) = c["checked"]["installs"].as_array() {
            for v in list {
                declared.insert(
                    v.as_str()
                        .expect("an installed name is a string")
                        .to_string(),
                );
            }
        }
    }
    assert!(
        !declared.is_empty(),
        "no component declares what it installs, so this measured nothing"
    );

    let dir = root().join("src/services");
    let mut modules: BTreeSet<String> = BTreeSet::new();
    for e in std::fs::read_dir(&dir)
        .expect("src/services exists")
        .flatten()
    {
        let p = e.path();
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if p.extension().and_then(|s| s.to_str()) != Some("rs") || stem == "mod" {
            continue;
        }
        modules.insert(stem.to_string());
    }
    assert!(
        !modules.is_empty(),
        "src/services has no module, so this measured nothing"
    );

    // A supervising module for every name declared. `tokenfuse-gateway` is
    // supervised by `gateway`, `tokenfuse-cloud` by `cloud`: the module is named
    // for its job inside this CLI, and the declared name is the one the estate
    // uses, so the match is on the module being MENTIONED by the declaration
    // rather than equal to it.
    for want in &declared {
        let short = want.rsplit('-').next().expect("a non-empty name");
        assert!(
            modules.contains(want.as_str())
                || modules.contains(&want.replace('-', "_"))
                || modules.contains(short),
            "components.json says this launcher installs `{want}` and src/services \
             has no module for it: {modules:?}"
        );
    }

    // And the other way, which is the half that catches a plane quietly gaining
    // a supervisor: every module that supervises something is declared.
    for m in &modules {
        if m == "tokenfuse_build" {
            continue;
        }
        let claimed = declared.iter().any(|d| {
            d == m || d.replace('-', "_") == *m || d.rsplit('-').next() == Some(m.as_str())
        });
        assert!(
            claimed,
            "src/services/{m}.rs supervises something and components.json does not \
             say this launcher installs it"
        );
    }
}

/// Every declared subcommand is one the argument parser knows.
#[test]
fn every_declared_subcommand_is_one_the_binary_dispatches_on() {
    let m = manifest();
    let cli = std::fs::read_to_string(root().join("src/cli.rs")).expect("reading cli.rs");

    let mut checked = 0;
    for c in components(&m) {
        let Some(subs) = c["checked"]["subcommands"].as_array() else {
            continue;
        };
        for s in subs {
            let s = s.as_str().expect("a subcommand is a string");
            let mut variant = s.to_string();
            variant[..1].make_ascii_uppercase();
            checked += 1;
            assert!(
                cli.contains(&format!("    {variant}("))
                    || cli.contains(&format!("    {variant},")),
                "components.json says {} takes `{s}` and cli.rs's Command enum has no {variant}",
                c["name"]
            );
        }
    }
    assert!(
        checked > 0,
        "no component declares a subcommand, so this measured nothing"
    );
}

/// It reads no environment variable of its own, and that is a claim rather than
/// an absence.
///
/// The reader is proved first against a planted name, so "found none" and
/// "cannot find any" are not the same result, and the walk skips this file for
/// the same reason qryx's does: the prover needs a name in its own source.
#[test]
fn it_reads_no_environment_of_its_own_and_the_reader_still_works() {
    let m = manifest();

    let planted = "TAIPAN_PLANTED";
    assert_eq!(
        names_in(&format!("let x = \"{planted}\";")),
        vec![planted.to_string()],
        "the reader cannot find a name in a string that contains exactly one, so \
         a finding of none below would prove nothing"
    );

    let mut found: Vec<String> = Vec::new();
    walk(&root().join("src"), &mut |p: &Path| {
        let s = p.to_string_lossy();
        if !s.ends_with(".rs") {
            return;
        }
        let Ok(body) = std::fs::read_to_string(p) else {
            return;
        };
        for n in names_in(&body) {
            found.push(format!(
                "{n} in {}",
                s.trim_start_matches(&*root().to_string_lossy())
            ));
        }
    });

    for c in components(&m) {
        if c["checked"]["reads_no_environment"].as_bool() != Some(true) {
            continue;
        }
        assert!(
            found.is_empty(),
            "components.json says this repository reads no environment variable, \
             and here they are: {found:?}"
        );
        let declared = c["checked"]["env"].as_object().map_or(0, |e| e.len());
        assert_eq!(
            declared, 0,
            "components.json claims reads_no_environment and also declares \
             {declared} variable(s). Those cannot both be true."
        );
    }
}

fn names_in(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let needle = b"TAIPAN_";
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let mut j = i + needle.len();
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            out.push(String::from_utf8_lossy(&bytes[i..j]).into_owned());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}
