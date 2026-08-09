#!/usr/bin/env bash
# Enforces invariant 1 of CLAUDE.md: no unwrap, expect or panic! in shipping
# code.
#
# taipan supervises other people's processes on their own machine. A panic
# leaves the supervised stack in an unknown state, which is worse than refusing
# to start: the operator has half a stack running and a tool that has already
# exited. Every fallible path returns Result instead.
#
# Test code is exempt, and deliberately so. unwrap in a test is a readable
# assertion; unwrap in the stop path is a crash on somebody's laptop.
#
# Scope: #[cfg(test)] modules, #[test] functions and files under tests/ are
# skipped. Lines carrying an explicit `// allow-panic:` comment are skipped too,
# so a genuinely infallible case can be waived in place, with a reason, rather
# than by disabling the gate.
#
# This file is the ONE copy of this check. The local hook calls it, and CI would
# call the same file if this repo ever gets CI.

set -euo pipefail

cd "$(dirname "$0")/.."

python3 - <<'PY'
import pathlib
import re
import sys

BANNED = re.compile(r"\.unwrap\(\)|\.expect\(|(?<![\w!])panic!\(|unimplemented!\(|todo!\(")

fail = False

# The subject first, for the same reason as the sibling check: an rglob over a
# directory that is not there yields nothing, and this printed "no unwrap,
# expect or panic in shipping code" and exited 0 having opened no file.
files = sorted(pathlib.Path("src").rglob("*.rs"))
if not files:
    print("FAIL: no .rs file under src/, so this measured nothing.")
    print("      It cannot say shipping code never panics if it read none.")
    print("      If the crate moved, this check has to move with it.")
    sys.exit(1)

for path in files:
    in_test_mod = False
    test_mod_depth = 0
    depth = 0
    prev_is_test_attr = False

    for lineno, line in enumerate(path.read_text().splitlines(), 1):
        stripped = line.strip()

        # Track brace depth so we know when a #[cfg(test)] module ends.
        opens = line.count("{")
        closes = line.count("}")

        if not in_test_mod and (
            stripped.startswith("#[cfg(test)]") or prev_is_test_attr and stripped.startswith("mod ")
        ):
            if stripped.startswith("#[cfg(test)]"):
                prev_is_test_attr = True
            else:
                in_test_mod = True
                test_mod_depth = depth
                prev_is_test_attr = False
            depth += opens - closes
            continue

        if prev_is_test_attr and not stripped.startswith("#["):
            # #[cfg(test)] applied to something other than a mod, e.g. a fn.
            in_test_mod = True
            test_mod_depth = depth
            prev_is_test_attr = False

        depth += opens - closes

        if in_test_mod:
            if depth <= test_mod_depth:
                in_test_mod = False
            continue

        if "// allow-panic:" in line:
            continue

        if BANNED.search(line):
            print(f"FAIL: {path}:{lineno}: {stripped[:90]}")
            fail = True

if fail:
    print()
    print("A supervisor that panics leaves the supervised stack in an unknown")
    print("state. Return a Result and let the caller decide, or, if the case is")
    print("genuinely infallible, waive the line with a `// allow-panic: <reason>`")
    print("comment so the reason travels with the code. See CLAUDE.md invariant 1.")
    sys.exit(1)

print("OK: no unwrap, expect or panic in shipping code.")
PY
