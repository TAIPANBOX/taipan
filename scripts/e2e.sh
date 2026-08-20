#!/usr/bin/env bash
# Invariant 5's end-to-end half: `up`, `up`, `down` against a real stack.
#
# WHY THIS IS NOT IN THE PRE-PUSH HOOK
#
# It starts real processes and binds the fixed ports 4100 and 8080. A hook that
# did that on every push would fight whatever the operator has open, including
# an environment they are in the middle of using, and a hook that fights the
# operator is a hook they disable with --no-verify. So it is run deliberately,
# and CLAUDE.md's Gates list says which one this is.
#
# WHAT IT NEEDS
#
# The tokenfuse sibling checkout, and either cached binaries in ~/.taipan/bin
# or the patience for a release build of that workspace. Nothing else: the
# gateway runs on its built-in stub unless --upstream is passed, and this
# never passes one, so no network call leaves the machine.
#
# WHY IT REFUSES RATHER THAN WAITS
#
# A busy port here is not congestion, it is somebody's environment. Waiting
# would mean eventually killing it, and this repository's whole subject is not
# touching processes it did not start.

set -uo pipefail

cd "$(dirname "$0")/.."

ports=(4100 8080)
busy=()
for p in "${ports[@]}"; do
	if nc -z 127.0.0.1 "$p" 2>/dev/null; then
		busy+=("$p")
	fi
done

if [ "${#busy[@]}" -ne 0 ]; then
	printf 'refusing to run: port(s) %s are already in use.\n' "${busy[*]}"
	printf '\n'
	printf 'This test binds the fixed money-plane ports, so something is already\n'
	printf 'holding them: most likely a taipan environment you have up. Stop it\n'
	printf 'with `taipan down --name <name>` and run this again.\n'
	printf '\n'
	printf 'It refuses rather than waiting on purpose. Waiting would end in taking\n'
	printf 'the port from a process this repository did not start, which is the one\n'
	printf 'thing invariant 2 exists to prevent.\n'
	exit 1
fi

# One at a time. Both tests bind 4100, so running them in parallel would have
# them fighting each other and reporting it as a product fault.
printf 'running the end-to-end tests (real processes, ports %s)\n\n' "${ports[*]}"
cargo test --test end_to_end -- --ignored --test-threads=1 --nocapture
rc=$?

printf '\n'
for p in "${ports[@]}"; do
	if nc -z 127.0.0.1 "$p" 2>/dev/null; then
		printf 'FAIL: port %s is still held after the tests finished.\n' "$p"
		printf '      Something was left running. Find the environment under\n'
		printf '      ~/.taipan/environments and stop it with `taipan down`.\n'
		rc=1
	fi
done

if [ "$rc" -ne 0 ]; then
	exit "$rc"
fi

printf 'OK: up is idempotent, down is complete, and nothing was left running.\n'
