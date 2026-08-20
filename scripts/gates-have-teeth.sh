#!/usr/bin/env bash
# Checks that the gates in `scripts/` still FAIL on the faults they exist to
# catch, still PASS on what they must not catch, and REFUSE to report success
# when they measured nothing at all.
#
# WHY
#
# Every gate here parses text, and a text parser does not break loudly: it
# stops matching and reports success. The mutants that proved each one existed
# as prose, in commit messages and in the `*(gate: ...)*` markers in CLAUDE.md,
# which is a record of what was true once. Nothing ran them again.
#
# A gate that has quietly stopped catching anything looks exactly like a gate
# with nothing to catch, and stays that way until the fault it guards ships.
#
# WHY THE THIRD PROPERTY IS SEPARATE FROM THE FIRST
#
# Because here it found both gates green over nothing, and this repository is
# the worst place in the estate for that: it has no CI, so `.githooks/pre-push`
# is the only thing that ever runs them. Nothing else would have said they had
# stopped checking.
#
# `no-process-scanning.sh` greps `src/` twice and `no-panic.sh` walks it with
# rglob. A grep over a directory that is not there prints nothing; an rglob over
# one yields nothing. Both exited 0 and printed a sentence asserting the
# opposite. Fixed in the commit before this one.
#
# HOW IT MUTATES WITHOUT LEAVING A MESS
#
# It edits tracked files in place, so it refuses to start unless the tree is
# clean, restores with `git checkout` after every case, restores again from a
# trap on any exit path including a kill, and asserts the tree is clean before
# reporting success.
#
#
# A GATE THAT IS ALREADY FAILING CANNOT BE JUDGED
#
# No case proves anything if the gate was already failing before the mutation.
# So every case runs the gate on the UNMUTATED tree first and reports
# UNJUDGEABLE. Found on 2026-08-09 in it-rat, where one gate was legitimately
# red and a case against it would have been indistinguishable from a working
# one.
#
# It covered only the fail-cases at first, which left the mirror of the same
# bug: on a red gate a pass-case reports OVEREAGER, "the gate failed on
# something it must not catch", and sends the reader to look at a harmless
# mutation. The verdict was being given without the predicate it depends on.
#
# A MUTATION THAT DID NOT APPLY PROVES NOTHING
#
# Every edit asserts it changed the file. A case whose edit applied nothing is
# a failure here, not a pass. That is not hypothetical: five such mutations
# were caught across idryx and tokenfuse on 2026-08-09, and three of the five
# had been verified BY HAND against the same gate minutes earlier. The hand
# version and the harness version differ only in how many layers of quoting sit
# between the text and python, which is exactly the difference nobody sees.

set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

# `--skip-if-dirty` is for `.githooks/pre-push`, and here it is load-bearing
# rather than a convenience: this repository has NO CI, so the hook is the only
# thing that ever runs any of this. Without the flag the choice would be
# between refusing every push that has uncommitted work in the tree, which is
# how a hook becomes something people disable, and not running this at all.
#
# The skip is loud, and it is the only exit 0 in this gate that did not check
# anything. Run it by hand after committing, which is what the hook prints.
skip_if_dirty=0
[ "${1:-}" = "--skip-if-dirty" ] && skip_if_dirty=1

if [ -n "$(git status --porcelain)" ]; then
	if [ "$skip_if_dirty" = 1 ]; then
		printf 'skipped: the tree is dirty, and this check mutates tracked files.\n'
		printf '         run ./scripts/gates-have-teeth.sh after committing.\n'
		exit 0
	fi
	printf 'this script mutates tracked files, so it needs a clean tree.\n'
	printf 'commit or stash first; it restores with `git checkout` and cannot\n'
	printf 'tell your edits from its own.\n'
	exit 1
fi

# Untracked files too: a mutation may RENAME a tracked file, and `git checkout`
# restores the original while leaving the new name behind. And the INDEX, since
# a gate may read `git ls-files` rather than the disk, so a mutation has to move
# the file in both. Safe because this
# script refuses to start unless the tree is clean, so anything untracked
# during a run was created by the run. `-x` is deliberately absent: ignored
# build output is not ours to delete.
restore() {
	git reset -q --hard HEAD 2>/dev/null
	git clean -fdq 2>/dev/null
}
baseline_dir="$(mktemp -d)"

# One trap for both, because a second `trap ... EXIT` REPLACES the first
# rather than adding to it. Writing them separately disarmed `restore` on
# every interrupt path, which would leave a mutated tree behind on Ctrl-C.
cleanup() {
	restore
	rm -rf "$baseline_dir"
}
trap cleanup EXIT INT TERM


failures=0
cases=0

# run_case <name> <expect: fail|pass> <gate> <python edit> [required output]
#
# The needle separates "it failed" from "it failed for the reason this case is
# about". Without it, a case expecting failure is satisfied by any failure,
# including one this harness caused itself.
run_case() {
	local name="$1" expect="$2" gate="$3" edit="$4" needle="${5:-}"
	cases=$((cases + 1))

	# The baseline applies to EVERY case, not only the ones expecting a failure.
	# It was `fail`-only until 2026-08-09, which left the mirror of the bug it was
	# written for: on a gate that is already red, a `pass` case reports OVEREAGER,
	# "the gate failed on something it must not catch", and sends the reader to
	# look at a harmless mutation while the gate was failing without it. Neither
	# verdict means anything on a red gate, so neither is given.
	skip_baseline=0
	if [ "$expect" = fail_env ]; then
		# `fail` with the baseline skipped, for cases whose fault IS the command
		# rather than a mutation: red before and after is the point there.
		expect=fail
		skip_baseline=1
	fi

	if [ "$skip_baseline" = 0 ]; then
		local key base_out
		key="$baseline_dir/$(printf '%s' "$gate" | cksum | tr -d ' ')"
		if [ ! -f "$key" ]; then
			if eval "$gate" >/dev/null 2>&1; then printf 'green' >"$key"; else printf 'red' >"$key"; fi
		fi
		base_out="$(cat "$key")"
		if [ "$base_out" = red ]; then
			printf 'UNJUDGEABLE  %s\n             the gate is already failing on a clean tree, so neither a\n             failure nor a pass after the mutation would prove anything\n' "$name"
			failures=$((failures + 1))
			return
		fi
	fi

	if ! python3 -c "$edit"; then
		printf 'BROKEN  %s\n        its mutation did not apply, so this case proved nothing\n' "$name"
		failures=$((failures + 1))
		restore
		return
	fi

	local out rc
	out=$(eval "$gate" 2>&1)
	rc=$?
	restore

	# Exit code first, then wording. Checking the needle before the expectation
	# turns "it did not fail at all" into "it failed for the wrong reason",
	# which sends the reader to look at prose when the gate is toothless.
	if [ "$expect" = fail ] && [ "$rc" -ne 0 ] && [ -n "$needle" ] &&
		! printf '%s' "$out" | grep -qF -- "$needle"; then
		printf 'WRONG REASON  %s\n              it failed, but not saying: %s\n' "$name" "$needle"
		failures=$((failures + 1))
		return
	fi
	if [ "$expect" = fail ] && [ "$rc" -eq 0 ]; then
		printf 'TOOTHLESS  %s\n           the gate passed on a fault it exists to catch\n' "$name"
		failures=$((failures + 1))
	elif [ "$expect" = pass ] && [ "$rc" -ne 0 ]; then
		printf 'OVEREAGER  %s\n           the gate failed on something it must not catch\n' "$name"
		failures=$((failures + 1))
		printf '%s\n' "$out" | head -4 | sed 's/^/           /'
	else
		printf 'ok  %-58s (%s)\n' "$name" "$expect"
	fi
}

py() { printf 'def edit(p, a, b):\n    s = open(p).read()\n    assert a in s, "pattern not found in " + p\n    open(p, "w").write(s.replace(a, b, 1))\n%s\n' "$1"; }

echo "=== faults each gate must catch ==="

# invariant 1: shipping code does not panic. An unwrap in a non-test path is
# the whole subject.
run_case "no-panic: an unwrap in shipping code" fail \
	'./scripts/no-panic.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[allow(dead_code)]\nfn _teeth() -> u32 { let v: Option<u32> = None; v.unwrap() }\n")')" \
	"unwrap"

# invariant 2: nothing asks the system what is running. Signalling a PID that
# did not come from our own pidfile is how a developer machine gets a stranger
# killed.
run_case "no-process-scanning: a process-discovery command" fail \
	'./scripts/no-process-scanning.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[allow(dead_code)]\nfn _teeth_scan() { let _ = std::process::Command::new(\"pgrep\"); }\n")')" \
	"FAIL"

run_case "no-process-scanning: libc::kill outside procutil" fail \
	'./scripts/no-process-scanning.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[allow(dead_code)]\nfn _teeth_kill(pid: i32) { unsafe { libc::kill(-pid, 0); } }\n")')" \
	"signals a process outside procutil"

# The scenarios exist so a reader can check what taipan promises without
# reading Rust. That only holds while every scenario is actually run. The
# realistic drift is not a deleted scenario, it is a RENAMED test: the rename
# is ordinary housekeeping, the scenario keeps reading like a specification,
# and nothing runs it any more.
run_case "scenarios-have-tests: a tag naming a test that no longer exists" fail \
	'./scripts/scenarios-have-tests.sh' \
	"$(py 'edit("src/workspace.rs", "fn finds_a_sibling_directly_under_the_workspace_root(", "fn finds_a_sibling_directly_under_the_workspace_root_renamed(")')" \
	"names no test under src/"

run_case "scenarios-have-tests: a scenario with no tag at all" fail \
	'./scripts/scenarios-have-tests.sh' \
	"$(py 'p = "features/the-seeded-demo-policy.feature"
s = open(p).read()
open(p, "w").write(s + "\n  Scenario: A promise nobody bound to a test\n    Given nothing holds this\n")')" \
	"no @test: tag"

# invariant 3: the direct set is what the invariant names. The realistic
# failure is not somebody sneaking a crate in, it is the document and the code
# drifting apart, which is why the allow-list is read out of CLAUDE.md.
run_case "declared-deps: a crate the invariant does not name" fail \
	'./scripts/declared-deps.sh' \
	"$(py 'edit("Cargo.toml", "[profile.release]", "# teeth: an undeclared crate.\nonce_cell = \"1\"\n\n[profile.release]")')" \
	"invariant 3 does not name it"

run_case "declared-deps: a named crate that Cargo.toml no longer declares" fail \
	'./scripts/declared-deps.sh' \
	"$(py 'edit("CLAUDE.md", "`libc`, `chrono`. Each is", "`libc`, `chrono`, `regex`. Each is")')" \
	"no longer declares it"

# The comment half holds one property and not the wider one it reads like.
# Deleting a comment in the MIDDLE merges its crates into the group above and
# is not caught, because a comment governs the run beneath it. What is caught
# is a crate sitting above every comment in the section. This case was written
# against the middle first, and this harness reported it TOOTHLESS, which is
# how the claim came to be narrowed instead of the check being trusted.
run_case "declared-deps: a crate above every comment in the section" fail \
	'./scripts/declared-deps.sh' \
	"$(py 'edit("Cargo.toml", "[dependencies]\n# CLI parsing.\n", "[dependencies]\nonce_cell = \"1\"\n# CLI parsing.\n")')" \
	"no comment above it"

# invariant 6: the promise is that no container runtime is needed. A single
# fallback that shells out to one fails only on the machine with no Docker,
# which is the machine this repo exists for.
run_case "no-docker: shelling out to a container runtime" fail \
	'./scripts/no-docker.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[allow(dead_code)]\nfn _teeth_docker() { let _ = std::process::Command::new(\"docker\"); }\n")')" \
	"invokes a container runtime"

run_case "no-docker: a Dockerfile appears in the tree" fail \
	'./scripts/no-docker.sh' \
	"$(py 'open("Dockerfile", "w").write("FROM scratch\n")')" \
	"a container file"

echo
echo "=== and what they must NOT catch ==="

# procutil owns every PID this program may touch, so libc::kill belongs THERE
# and a gate that flagged it would be flagging the design it protects.
run_case "no-process-scanning: libc::kill inside procutil, where it belongs" pass \
	'./scripts/no-process-scanning.sh' \
	"$(py 'p = "src/procutil.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[allow(dead_code)]\nfn _teeth_ok(pid: i32) { let _ = unsafe { libc::kill(-pid, 0) }; }\n")')"

# A test may unwrap. Flagging that would make the gate something people switch
# off, and then the shipping-code case goes through with it.
run_case "no-panic: an unwrap inside a test module" pass \
	'./scripts/no-panic.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[cfg(test)]\nmod teeth_tests {\n    #[test]\n    fn t() { let v: Option<u32> = Some(1); assert_eq!(v.unwrap(), 1); }\n}\n")')"

# Only ONE direction is checked, on purpose. A unit test without a scenario is
# ordinary: not every assertion is a promise to an operator. A gate that
# demanded a scenario per test would make people delete scenarios to keep it
# quiet, and the real ones would go with them.
run_case "scenarios-have-tests: a unit test with no scenario, which is allowed" pass \
	'./scripts/scenarios-have-tests.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n#[cfg(test)]\nmod teeth_unbound {\n    #[test]\n    fn an_assertion_that_is_not_a_promise() { assert_eq!(1 + 1, 2); }\n}\n")')"

# One comment covers a pair when the pair is one decision. The first version of
# this gate reset after every line and accused serde_json and
# tracing-subscriber, which is correct code flagged as a fault.
run_case "declared-deps: two crates under one comment, which is one decision" pass \
	'./scripts/declared-deps.sh' \
	"$(py 'edit("Cargo.toml", "serde_json = \"1\"", "serde_json = \"1.0\"")')"

# The word Docker belongs in a comment that promises not to use one. A gate
# failing on the sentence that states the invariant gets switched off.
run_case "no-docker: a comment naming Docker to say we do not use it" pass \
	'./scripts/no-docker.sh' \
	"$(py 'p = "src/cli.rs"
s = open(p).read()
open(p, "w").write(s + "\n// No docker run here: invariant 6 is the reason this binary exists.\n")')"

echo
echo "=== and the one this estate learned the hard way ==="
echo "    a gate whose subject is gone must SAY so, not report OK on nothing"

# THE HOLE, both halves of it. Renaming the crate emptied both checks while
# each printed that the code was clean.
run_case "no-panic: no Rust left under src/" fail \
	'./scripts/no-panic.sh' \
	"$(py 'import subprocess
subprocess.run(["git", "mv", "src", "src-elsewhere"], check=True)')" \
	"measured nothing"

run_case "no-process-scanning: no Rust left under src/" fail \
	'./scripts/no-process-scanning.sh' \
	"$(py 'import subprocess
subprocess.run(["git", "mv", "src", "src-elsewhere"], check=True)')" \
	"measured nothing"

run_case "scenarios-have-tests: no feature file left" fail \
	'./scripts/scenarios-have-tests.sh' \
	"$(py 'import subprocess
subprocess.run(["git", "mv", "features", "features-elsewhere"], check=True)')" \
	"measured nothing"

# BOTH homes, since 2026-08-20. The gate reads src/ and tests/, so moving one
# away leaves it reading the other and the "measured nothing" path is never
# reached. This case moved only src/ until the integration tests arrived, and
# would have quietly stopped testing what it names.
run_case "scenarios-have-tests: no Rust left under src/ or tests/" fail \
	'./scripts/scenarios-have-tests.sh' \
	"$(py 'import subprocess
subprocess.run(["git", "mv", "src", "src-elsewhere"], check=True)
subprocess.run(["git", "mv", "tests", "tests-elsewhere"], check=True)')" \
	"measured nothing"

run_case "declared-deps: no CLAUDE.md, so no allow-list at all" fail \
	'./scripts/declared-deps.sh' \
	"$(py 'import subprocess
subprocess.run(["git", "mv", "CLAUDE.md", "CLAUDE-elsewhere.md"], check=True)')" \
	"measured nothing"

run_case "declared-deps: invariant 3 no longer says what it said" fail \
	'./scripts/declared-deps.sh' \
	"$(py 'edit("CLAUDE.md", "3. **Dependencies stay at the declared set**:", "3. **Dependencies are whatever they are**:")')" \
	"measured nothing"

run_case "no-docker: no Rust left under src/" fail \
	'./scripts/no-docker.sh' \
	"$(py 'import subprocess
subprocess.run(["git", "mv", "src", "src-elsewhere"], check=True)')" \
	"measured nothing"

echo
if [ -n "$(git status --porcelain)" ]; then
	printf 'FAIL: this script left the tree dirty, so it cannot be trusted about anything above\n'
	git status --porcelain | head -5
	exit 1
fi

if [ "$failures" -gt 0 ]; then
	printf '%d of %d cases failed.\n' "$failures" "$cases"
	printf 'A gate that has quietly stopped catching anything looks exactly like a gate\n'
	printf 'with nothing to catch, and stays that way until the fault it guards ships.\n'
	exit 1
fi

printf 'OK: %d cases. Every gate fails on its own fault, passes on a non-fault,\n' "$cases"
printf '    and refuses to report success when it measured nothing.\n'
