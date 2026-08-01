#!/usr/bin/env bash
# Enforces invariant 2 of CLAUDE.md: stop by process group, never by a PID
# discovered by scanning.
#
# taipan signals process groups it created itself, by a PID captured at spawn
# time and written to its own pidfile. It never asks the system what is running.
#
# The way that gets lost is a stubborn shutdown. Something does not die, and the
# obvious fix is to look up whatever holds the port and kill that. On a
# developer machine, whatever holds the port is regularly not our process at
# all, and the failure is silent in the worst direction: it works every time
# until the one time it kills somebody's editor, database, or a colleague's
# service on a shared box.
#
# The check is on the SOURCE, not on a running binary, because the point is to
# refuse the edit rather than to catch the behaviour afterwards.
#
# `ss` is deliberately matched only as a whole word in a command position:
# it is two letters and would otherwise fire on every identifier containing it.
#
# This file is the ONE copy of this check.

set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

problems=0

# A process-discovery tool named as a command to run.
BANNED='Command::new\("(ps|lsof|ss|pgrep|pidof|fuser|netstat)"\)|"(pgrep|pidof|lsof|fuser|netstat)"'

while IFS= read -r hit; do
	[ -n "$hit" ] || continue
	file=${hit%%:*}
	case "$file" in
	*/tests/* | *_test.rs) continue ;;
	esac
	echo "FAIL: $hit"
	problems=$((problems + 1))
done < <(grep -rnE "$BANNED" --include='*.rs' src/ 2>/dev/null)

# Signalling a PID that did not come from our own pidfile. libc::kill is
# expected in procutil and nowhere else; anywhere else means somebody found a
# process another way and is about to signal it.
while IFS= read -r hit; do
	[ -n "$hit" ] || continue
	file=${hit%%:*}
	case "$file" in
	src/procutil.rs) continue ;;
	esac
	echo "FAIL: $hit signals a process outside procutil, which owns every PID this program is allowed to touch"
	problems=$((problems + 1))
done < <(grep -rn 'libc::kill' --include='*.rs' src/ 2>/dev/null)

if [ "$problems" -ne 0 ]; then
	echo
	echo "Signal the group taipan created, by the PID it captured at spawn. Asking"
	echo "the system what is running finds whatever holds the port right now, which"
	echo "on a developer machine is regularly not our process."
	echo "See CLAUDE.md invariant 2."
	exit 1
fi

echo "OK: no process discovery, and every signal goes through procutil."
