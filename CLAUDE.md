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
./scripts/no-process-scanning.sh
./scripts/gates-have-teeth.sh     # invariant 7; needs a clean tree
```

`no-process-scanning.sh` was missing from this list until 2026-08-09 while the
hook ran it, so "run every gate below" was a smaller instruction than the hook's.

## Running the gates

```sh
git config core.hooksPath .githooks   # once, per clone
```

There is no CI in this repository, so `.githooks/pre-push` is the ONLY thing
that runs the gates above. Without that one line they are scripts nobody calls,
which is a comment with an exit code. `git push --no-verify` skips them, and
should be rare enough to be worth explaining.

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
   on a developer machine is regularly not our process at all.
   *(gate: `scripts/no-process-scanning.sh`, which also requires every
   `libc::kill` to live in `procutil`, the module that owns every PID this
   program may touch)*
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
   untouched.
   *(partly gated: `stop_group_actually_removes_the_group`,
   `stopping_twice_is_a_no_op_not_an_error`,
   `a_group_that_ignores_the_primary_signal_is_force_killed` hold the signalling
   half against real process groups. The end-to-end half, `taipan up` twice
   against a built stack, is untested and needs the stack built.)*
7. **Every gate here is proven able to fail, by planting its fault and requiring
   the failure.** A gate that has quietly stopped catching anything looks exactly
   like a gate with nothing to catch, and in this repository nothing else would
   ever say so: there is no CI, and the hook is the only thing that runs them.

   Both gates were found green over nothing on 2026-08-09. `no-process-scanning.sh`
   greps `src/` twice and `no-panic.sh` walks it with rglob; a grep over a missing
   directory prints nothing and an rglob over one yields nothing, so both exited 0
   and printed that the code was clean having read none of it. Renaming or moving
   the crate is ordinary housekeeping.
   *(gate: `scripts/gates-have-teeth.sh`, 7 cases: three planted faults, two
   non-faults that must NOT fire, and both subjects taken away. The non-faults are
   the ones worth keeping: `libc::kill` inside `procutil.rs` is exactly where
   invariant 2 puts it, and an `unwrap` inside a `#[cfg(test)]` module is allowed
   by invariant 1. A gate that flagged either would be switched off, and the real
   cases would go with it.)*

   In the hook it takes `--skip-if-dirty`, the only exit 0 here that checked
   nothing. It mutates tracked files, and with no CI the alternative was refusing
   every push with uncommitted work in the tree, which is how a hook becomes
   something people disable. The skip prints why.

6. **No Docker.** That is the entire reason this exists next to `stack-single`.
   A dependency that needs a container runtime defeats the point.
   *(not enforced)*

## Decisions that have no gate yet

This list is debt, and it is here to stay visible rather than to be tidy.

**Held by this file alone: invariants 3, 4 and 6.** Invariant 5 is half
held.

- **Invariant 2** is now `scripts/no-process-scanning.sh`. It checks the SOURCE
  rather than a running binary, because the point is to refuse the edit. The way
  this invariant gets lost is a stubborn shutdown: something will not die, and
  the obvious fix is to look up whatever holds the port. That works every time
  until the one time it kills somebody's editor or a colleague's service on a
  shared box. Verified by breaking twice: an `lsof` lookup, and a `libc::kill`
  outside `procutil`. The regression mode is
  somebody "fixing" a stubborn shutdown by looking up whatever holds the port.
- **Invariant 3** is a dependency allow-list, the same shape as the one in
  `mockryx`, perhaps thirty lines.
- **Invariant 5**'s signalling half is now three tests against real process
  groups: a stopped group is actually gone, stopping twice is a no-op rather
  than an error, and a group that ignores SIGTERM is escalated to SIGKILL and
  reported as force-killed. They use `group_alive`, never `ps` or `lsof`, so
  they obey invariant 2 rather than checking it through a mechanism this repo
  forbids.

  **A trap worth knowing before writing more of these.** The first version
  never reaped its children. `group_alive` probes with `kill(-pid, 0)`, and an
  unreaped dead child is a zombie: the entry still exists, the probe returns
  `EPERM` not `ESRCH`, and `group_alive` reads `EPERM` as alive deliberately,
  because "I cannot tell" must fail closed. The tests watched a corpse read as
  living. Production never meets this, because `up` exits and init reaps, so
  any test that is itself the parent must reap on a thread.

  The half still missing is end-to-end: `up`, `up`, `down` against a built
  stack, asserting no listener remains and no stale pidfile is left. That needs
  the four products built, which is why it is not here yet.

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
