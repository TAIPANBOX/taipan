# Security Policy

taipan is the native, no-Docker process supervisor for the TAIPANBOX
agent-governance stack: `taipan up` builds or reuses sibling service
binaries and starts and stops other people's processes on their own machine,
so its trust boundary is that machine and the descriptor other tools
auto-discover from it.

## Reporting a vulnerability

Please report security issues privately, not in public issues or pull
requests: open a GitHub private security advisory at
<https://github.com/TAIPANBOX/taipan/security/advisories/new>. Include the
affected version or commit, a description and a minimal reproduction. We aim
to acknowledge within a few days and to fix high-severity issues before any
public disclosure, with coordinated disclosure within 90 days of the report.
There is no bug-bounty programme; reporters are credited in the advisory
unless they prefer otherwise.

## Supported versions

Before this repository's 1.0, only `main` is supported: fixes land on `main`
and are not backported. From its 1.0 tag, the newest minor gets every fix and
the previous minor gets security-relevant fixes for 90 days after the newer
one is tagged.

## Verifying a build

Every change passes the repository's gates before merge:
`cargo fmt --all -- --check`, `cargo clippy --all-targets`,
`cargo test --all`, `./scripts/no-panic.sh`,
`./scripts/no-process-scanning.sh`, `./scripts/scenarios-have-tests.sh`,
`./scripts/declared-deps.sh`, `./scripts/no-docker.sh` and
`./scripts/gates-have-teeth.sh`. There is no CI in this repository;
`.githooks/pre-push` is what runs them.
