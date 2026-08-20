#!/usr/bin/env bash
# Enforces invariant 6 of CLAUDE.md: no Docker.
#
# That is the entire reason this binary exists beside `stack-single`. An
# operator runs `taipan up` precisely because they do not want to install a
# container runtime first, and a single `Command::new("docker")` on a fallback
# path takes the promise away without breaking a single test: it fails only on
# the machine that does not have Docker, which is the machine this repo is for.
#
# WHAT IT LOOKS AT, AND WHY NOT THE PROSE
#
# Only `src/` and the repository's own file names. The word "Docker" belongs in
# README.md, in CLAUDE.md and in the crate description, where it appears as a
# promise NOT to use one. A gate that grepped the prose would fail on the
# sentence that states the invariant, which is the fastest way to get a gate
# switched off.
#
# Dependencies are not checked here. `scripts/declared-deps.sh` caps the direct
# set at what invariant 3 names, so a container-runtime crate cannot arrive
# without that gate failing first, and duplicating the check would leave two
# copies to drift.
#
# This file is the ONE copy of this check. There is no CI in this repository,
# so `.githooks/pre-push` is the only thing that runs it.

set -euo pipefail

cd "$(dirname "$0")/.."

python3 - <<'PY'
import pathlib
import re
import sys

# Runtimes, not the word. `docker` as a bare word appears in comments that
# explain the invariant; what matters is invoking one.
RUNTIMES = ("docker", "docker-compose", "podman", "nerdctl", "containerd", "colima")
INVOKE = re.compile(
    r"""Command::new\(\s*"(%s)"|
        \b(%s)\s+(?:run|build|compose|ps|exec|pull)\b"""
    % ("|".join(RUNTIMES), "|".join(RUNTIMES)),
    re.X,
)

fail = False

# The subject first, for the reason the rest of this estate learned on
# 2026-08-09: an rglob over a directory that is not there yields nothing, and a
# check that read no file must never report that everything it did not read was
# clean.
sources = sorted(pathlib.Path("src").rglob("*.rs"))
if not sources:
    print("FAIL: no .rs file under src/, so this measured nothing.")
    print("      It cannot say taipan never shells out to a container runtime")
    print("      if it read no source. If the crate moved, so must this check.")
    sys.exit(1)

for path in sources:
    for lineno, line in enumerate(path.read_text().splitlines(), 1):
        stripped = line.strip()
        if stripped.startswith("//"):
            # A comment may name Docker to say taipan does not use it.
            continue
        if INVOKE.search(line):
            print(f"FAIL: {path}:{lineno}: invokes a container runtime")
            print(f"      {stripped[:90]}")
            fail = True

# A Dockerfile or a compose file in the tree is the same promise broken by a
# different route: it says the supported way to run this needs a runtime.
CONTAINER_FILES = re.compile(
    r"(^|/)(Dockerfile[^/]*|docker-compose\.ya?ml|compose\.ya?ml|\.dockerignore)$"
)
tracked = [
    p
    for p in pathlib.Path(".").rglob("*")
    if p.is_file() and ".git/" not in str(p) and not str(p).startswith("target/")
]
if not tracked:
    print("FAIL: no file found in the repository at all, so this measured nothing.")
    sys.exit(1)

for p in tracked:
    if CONTAINER_FILES.search(str(p)):
        print(f"FAIL: {p}: a container file, and this repo exists to not need one")
        fail = True

if fail:
    print()
    print("taipan exists beside stack-single precisely so an operator does not have")
    print("to install a container runtime first. See CLAUDE.md invariant 6. A")
    print("fallback that shells out to Docker fails only on the machine this repo")
    print("is for, which is why no test would catch it.")
    sys.exit(1)

print(f"OK: {len(sources)} source files, no container runtime invoked, no container file.")
PY
