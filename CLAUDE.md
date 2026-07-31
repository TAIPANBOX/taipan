# CLAUDE.md, working instructions for taipan

These instructions apply to any model working in this repo. Read this file
before writing code. It holds process and invariants only: **no status.**
Status goes stale, and a stale instruction file is worse than none.

## Read before you change anything

1. `README.md`, for what `taipan up` promises an operator.
2. `Cargo.toml`. The dependency comments there explain why each crate is
   present, and they are the source for invariant 3.
3. The descriptor format, because the Genaryx console auto-discovers it. It is
   a contract with another repo, not an internal detail.

## What this is

A native, no-Docker, one-command supervisor for the agent-governance stack.
`taipan up` builds or starts the services and writes a descriptor the Genaryx
console discovers on its own. This repo is **private**.

## Blast radius

This binary starts and stops other people's processes on their own machine. The
two ways to make that dangerous are killing the wrong process and leaving a
half-started stack behind, and both invariants below exist for exactly those.

The descriptor is read by Genaryx. Changing its shape breaks discovery in a
different repo that will not fail loudly, it will simply find nothing.

## The working loop

1. Branch off `main`, one logical increment per branch.
2. Run every gate below. All must pass locally before the push.
3. Commit with Conventional Commits. End the message with the standard
   co-author trailer naming the model that actually did the work.
4. Push the branch, open a PR with `gh`.
5. **Ask the user before merging.** Do not self-merge.

There is no CI in this repo, so the local gates are the only gates. Treat a
skipped local run as a skipped review.

## Gates

```sh
cargo fmt --all -- --check
cargo clippy --all-targets
cargo test --all
./scripts/no-panic.sh
```

## Hard invariants

Each one carries how it is held today. Use `(gate: ...)`, `(test: ...)`,
`(partly gated: ...)` or `(not enforced)`, and use the weakest one that is
true. An invariant with no check, written as though it had one, is worse than
an absent invariant.

1. **Every fallible path outside tests returns `Result`. No `unwrap`, no
   `expect`, no `panic!` in shipping code.** A supervisor that panics leaves the
   thing it supervises in an unknown state, which is the one outcome worse than
   refusing to start. *(gate: `scripts/no-panic.sh`)*
2. **Stop by process group, never by a PID discovered by scanning.** Signal the
   group taipan itself created. Never derive a target from `ps`, `lsof`, `ss` or
   a port lookup: those find whatever happens to hold the port right now, which
   on a developer machine is regularly not our process at all. *(not enforced)*
3. **Dependencies stay at the declared set**: `clap`, `serde`, `serde_json`,
   `anyhow`, `tracing`, `tracing-subscriber`, `libc`. Each is justified by a
   comment in `Cargo.toml`; a new one needs the user, and a new comment saying
   why. *(not enforced)*
4. **The descriptor is a cross-repo contract.** Genaryx auto-discovers it. A
   field rename or a path change is a coordinated change with that repo, and
   its failure mode is silence, not an error. *(not enforced)*
5. **`up` is idempotent and `down` is complete.** Running `up` twice must not
   start a second copy or corrupt the pidfile, and `down` must leave nothing
   holding a port. The second run is the real test: works twice, from empty,
   untouched. *(not enforced)*
6. **No Docker.** That is the entire reason this exists next to `stack-single`.
   A dependency that needs a container runtime defeats the point.
   *(not enforced)*

## Decisions that have no gate yet

This list is debt, and it is here to stay visible rather than to be tidy.

**Held by this file alone: invariants 2, 3, 4, 5 and 6.**

- **Invariant 2** is checkable and worth it: fail if the source contains a call
  to `ps`, `lsof` or `ss` anywhere in the stop path. The regression mode is
  somebody "fixing" a stubborn shutdown by looking up whatever holds the port.
- **Invariant 3** is a dependency allow-list, the same shape as the one in
  `mockryx`, perhaps thirty lines.
- **Invariant 5** is the highest value and the hardest: an integration test that
  runs `up`, `up`, `down`, then asserts no listener remains and no stale pidfile
  is left. Everything about this repo's promise lives in that test.

## Standing rule

An approved architecture decision is **not finished** until it is two things: a
numbered invariant in this file, and a gate in a script if it can be checked
structurally. Until then it is a document, and documents do not stop code.

## Escalate, do not push through

- Any change to the descriptor shape or location, because Genaryx reads it.
- Anything in the process stop path.
- Adding a dependency.
- Cutting a tag or making this repo public.

## Conventions

- **No long dashes** anywhere: not in code comments, docs, commit messages, or
  PR bodies. Use a comma, a colon, parentheses, or a short hyphen.
- Nothing paid or metered gets enabled without telling the user first. This repo
  is private, so enabling CI here would start metering Actions minutes: ask
  before adding a workflow.
- Do not delete or revoke keys, tokens, or certificates on your own initiative.
