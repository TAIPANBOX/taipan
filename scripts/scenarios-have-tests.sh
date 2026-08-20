#!/usr/bin/env bash
# Binds features/*.feature to the tests that hold them.
#
# The scenarios exist so a reader can check what taipan promises without reading
# Rust. That only works while every scenario is actually held by a test: a
# scenario nobody runs is a nicer-looking comment, and it rots the same way, only
# with more authority because it reads like a specification.
#
# Two directions, both checked:
#   1. every Scenario carries a @test: tag;
#   2. every @test: tag names a #[test] fn that exists under src/.
#
# The reverse of 2 is deliberately NOT checked. A unit test without a scenario is
# fine and common: not every assertion is a promise to an operator.
#
# There is no CI in this repository, so the local hook is the only thing that
# runs this.

set -euo pipefail

cd "$(dirname "$0")/.."

python3 - <<'PY'
import pathlib
import re
import sys

SCENARIO = re.compile(r"^\s*Scenario:\s*(.+?)\s*$")
TAG = re.compile(r"^\s*@test:([A-Za-z_][A-Za-z0-9_]*)\s*$")
TEST_FN = re.compile(r"^\s*(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")

fail = False

# The subject first. A glob over a directory that is not there yields nothing,
# and a check that read no file must never report that everything it did not
# read was fine. Both subjects are taken away by scripts/gates-have-teeth.sh.
features = sorted(pathlib.Path("features").glob("*.feature"))
if not features:
    print("FAIL: no .feature file under features/, so this measured nothing.")
    print("      It cannot say every scenario is held by a test if it read none.")
    print("      If the scenarios moved, this check has to move with them.")
    sys.exit(1)

# Both homes. Unit tests live beside the code in src/; integration tests live
# in tests/, and invariant 5's end-to-end scenarios are bound to those. Reading
# only src/ reported them unbound on 2026-08-20, which is this gate being right
# about its scope and wrong about the repository.
sources = sorted(pathlib.Path("src").rglob("*.rs")) + sorted(
    pathlib.Path("tests").rglob("*.rs")
)
if not sources:
    print("FAIL: no .rs file under src/ or tests/, so this measured nothing.")
    print("      Every tag would look unresolved, which is a different fault")
    print("      wearing the same message. If the crate moved, so must this.")
    sys.exit(1)

# Every function name that carries #[test] on a preceding attribute line.
known_tests = set()
for path in sources:
    pending_test_attr = False
    for line in path.read_text().splitlines():
        stripped = line.strip()
        if stripped == "#[test]":
            pending_test_attr = True
            continue
        if pending_test_attr:
            m = TEST_FN.match(line)
            if m:
                known_tests.add(m.group(1))
                pending_test_attr = False
            elif stripped.startswith("#["):
                # Another attribute between #[test] and the fn, e.g. #[ignore].
                continue
            else:
                pending_test_attr = False

if not known_tests:
    print("FAIL: no #[test] function found under src/, so this measured nothing.")
    sys.exit(1)

tagged = 0
for path in features:
    pending_tag = None
    for lineno, line in enumerate(path.read_text().splitlines(), 1):
        m = TAG.match(line)
        if m:
            pending_tag = (m.group(1), lineno)
            continue
        m = SCENARIO.match(line)
        if not m:
            continue
        scenario = m.group(1)
        if pending_tag is None:
            print(f"FAIL: {path}:{lineno}: scenario has no @test: tag above it")
            print(f"      {scenario}")
            fail = True
            continue
        name, tag_line = pending_tag
        tagged += 1
        if name not in known_tests:
            print(f"FAIL: {path}:{tag_line}: @test:{name} names no test under src/ or tests/")
            print(f"      scenario: {scenario}")
            fail = True
        pending_tag = None

if fail:
    print()
    print("A scenario nobody runs is a comment that reads like a specification.")
    print("Either write the test and name it in the tag, or delete the scenario.")
    sys.exit(1)

print(
    f"OK: {tagged} scenario(s) across {len(features)} feature file(s), "
    f"every one bound to a test that exists."
)
PY
