#!/usr/bin/env bash
# Enforces invariant 3 of CLAUDE.md: the direct dependencies stay at the
# declared set, and each one is justified by a comment in Cargo.toml.
#
# WHY THE ALLOW-LIST IS NOT IN THIS FILE
#
# It is read out of CLAUDE.md invariant 3, which is the only copy. A gate
# carrying its own list would be a second copy of the same decision, and the
# two would drift: on 2026-08-20 invariant 3 named seven crates while
# Cargo.toml declared eight. `chrono` had been added, with a proper
# justifying comment, and the invariant was never updated. Nothing said so,
# because the invariant was marked `(not enforced)` and there was nothing to
# say it.
#
# So the failure this gate exists for is not really "somebody added a crate".
# It is "the document and the code stopped agreeing", and the fix is to make
# the document the input rather than a parallel description.
#
# WHY DIRECT ONLY
#
# Transitive crates are pulled by these eight and are not ours to choose.
# Pinning them here would go stale on the next `cargo update` and teach the
# reader to ignore this check.
#
# This file is the ONE copy of this check. There is no CI in this repository,
# so `.githooks/pre-push` is the only thing that runs it.

set -euo pipefail

cd "$(dirname "$0")/.."

python3 - <<'PY'
import json
import pathlib
import re
import subprocess
import sys

fail = False

# --- the allow-list, read out of the invariant itself -----------------------
claude = pathlib.Path("CLAUDE.md")
if not claude.is_file():
    print("FAIL: CLAUDE.md is not here, so this measured nothing.")
    print("      The allow-list lives in invariant 3 and there is no other copy.")
    sys.exit(1)

text = claude.read_text()
m = re.search(
    r"^3\.\s+\*\*Dependencies stay at the declared set\*\*:(.+?)(?=\n\d+\.\s+\*\*|\n##\s)",
    text,
    re.S | re.M,
)
if not m:
    print("FAIL: could not find invariant 3 in CLAUDE.md, so this measured nothing.")
    print("      It reads the allow-list from that invariant. If the wording")
    print("      changed, this parser has to change with it, deliberately.")
    sys.exit(1)

# Everything backticked up to the end of the sentence that lists them.
listed = m.group(1).split(".")[0]
allowed = set(re.findall(r"`([a-zA-Z0-9_-]+)`", listed))
if not allowed:
    print("FAIL: invariant 3 names no crate, so this measured nothing.")
    sys.exit(1)

# --- what the crate actually declares ---------------------------------------
try:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
    print(f"FAIL: could not read cargo metadata, so this measured nothing: {e}")
    sys.exit(1)

declared = set()
for pkg in meta.get("packages", []):
    for dep in pkg.get("dependencies", []):
        # Dev- and build-dependencies are a different decision; invariant 3 is
        # about what ships. There are none today, and if one appears it should
        # arrive with its own line in the invariant rather than silently here.
        if dep.get("kind") in (None, "normal"):
            declared.add(dep["name"])

if not declared:
    print("FAIL: cargo reports no direct dependency at all, so this measured nothing.")
    sys.exit(1)

for extra in sorted(declared - allowed):
    print(f"FAIL: '{extra}' is a direct dependency and invariant 3 does not name it")
    fail = True
for gone in sorted(allowed - declared):
    print(f"FAIL: invariant 3 names '{gone}' and Cargo.toml no longer declares it")
    print("      Either it was dropped on purpose, in which case update the")
    print("      invariant, or the invariant is describing a repo that is gone.")
    fail = True

# --- and each one justified where a reader will look ------------------------
# Invariant 3 does not only cap the count, it says each crate is "justified by
# a comment in Cargo.toml". A list with no reasons is a list nobody can audit.
#
# WHAT THIS HALF CAN AND CANNOT CATCH, because the difference is not obvious
# and gates-have-teeth.sh found it rather than a reader.
#
# A comment governs the RUN of lines beneath it, because `serde`/`serde_json`
# and `tracing`/`tracing-subscriber` are one decision each. That rule and
# "every crate has its own comment" cannot both hold: deleting one comment
# merely merges its crates into the group above, and the result still satisfies
# the run rule. So this catches a crate that sits above EVERY comment in the
# section, and it does not catch a deleted comment in the middle. Narrowed to
# the true claim on 2026-08-20 rather than left asserting the wider one, which
# the harness showed was not held.
cargo_toml = pathlib.Path("Cargo.toml").read_text().splitlines()
try:
    start = cargo_toml.index("[dependencies]")
except ValueError:
    print("FAIL: no [dependencies] section in Cargo.toml, so this measured nothing.")
    sys.exit(1)

# A comment governs the RUN of dependency lines that follows it, up to the
# next comment, blank line or section. That is how the file is written and how
# a person reads it: `serde` and `serde_json` are one decision under one
# sentence, and so are `tracing` and `tracing-subscriber`. Resetting after each
# line instead flagged both of the second crates, which is the OVEREAGER shape
# this estate deletes gates for. A gate that fires on correct code gets
# switched off, and the real cases go with it.
seen_comment = False
justified = set()
for line in cargo_toml[start + 1 :]:
    stripped = line.strip()
    if stripped.startswith("["):
        break
    if stripped.startswith("#"):
        seen_comment = True
        continue
    if not stripped:
        seen_comment = False
        continue
    dep = re.match(r"([a-zA-Z0-9_-]+)\s*=", stripped)
    if dep and seen_comment:
        justified.add(dep.group(1))

for dep in sorted(declared - justified):
    print(f"FAIL: '{dep}' has no comment above it in Cargo.toml saying why it is here")
    fail = True

if fail:
    print()
    print("A new dependency needs the user, and a comment saying why. See CLAUDE.md")
    print("invariant 3. The allow-list is that invariant, so updating one updates")
    print("both by construction.")
    sys.exit(1)

print(
    f"OK: {len(declared)} direct dependencies, exactly those invariant 3 names, "
    "each with a reason beside it."
)
PY
