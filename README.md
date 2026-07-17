# taipan

`taipan` is a native, no-Docker process supervisor for the
[TAIPANBOX](https://github.com/TAIPANBOX) agent-governance stack. `taipan up`
builds (or reuses) the service binaries from your sibling checkouts, starts
them with a fixed local port map, waits for each to report healthy, and
writes a descriptor that other tools (the Genaryx console, in particular) can
auto-discover. `taipan down` stops exactly what it started, cleanly, with no
orphaned processes.

It generalizes the pattern proven in
[`bank-in-a-box/run.sh`](https://github.com/TAIPANBOX/bank-in-a-box) into a
reusable CLI.

This is the private Genaryx orchestrator: the descriptor, auto-discovery,
enforcement presets and `--with` wiring that make the Genaryx console light up
over a real stack. The free, minimal "run the open stack locally" launcher for
adopters lives in the public [stack-up](https://github.com/TAIPANBOX/stack-up)
repo instead. The source here stays Apache-2.0 (see [LICENSE](LICENSE)); the
repo is private as part of the Genaryx product boundary.

## What it starts

By default, `taipan up` brings up the money plane only:

| Service | Port | Notes |
|---|---|---|
| TokenFuse gateway | 4100 | budget enforcement proxy; loopback only, no caller-facing auth |
| TokenFuse Cloud | 8080 | control API; dev bearer keys minted per environment |

`--with wardryx,idryx` adds:

| Service | Port | Notes |
|---|---|---|
| Wardryx | 8090 | policy decision point; seeded with a demo policy scoped to `agent://mockryx.local/*` (a small `require_human_above_usd` and a `deny_tool: [shell_exec]`) and a minted `WARDRYX_APPROVAL_SECRET`; the gateway is wired to consult it (`TOKENFUSE_WARDRYX_MODE=enforce`) |
| Idryx | 8081 | identity graph; its own default (:8080) collides with Cloud, hence the remap |

Every service binds to `127.0.0.1` only.

## Requirements

- Rust (stable) and Go, on `PATH`.
- Sibling checkouts of `tokenfuse` (always) and, if you pass `--with`,
  `wardryx` and/or `Idryx` — either next to your `taipan` checkout, or next to
  its parent directory. `taipan` tries both locations before giving up; pass
  `--workspace <dir>` to point at a different parent directory entirely.
  taipan never modifies these repos; it only reads their source (to decide
  whether a rebuild is needed) and runs their own build tool
  (`cargo build --release` / `go build`) with output redirected into
  `~/.taipan/bin/`.

## Usage

```sh
# Bring up the money plane, wait for /healthz, write the descriptor.
taipan up --name demo

# Add the policy and identity planes.
taipan up --name demo --with wardryx,idryx

# Point at sibling checkouts that live somewhere other than . or ..
taipan up --name demo --workspace ~/Development

# Dev mode: Cloud runs on the devkey fallback instead of minted keys, so an
# auto-discovering console can pair without hitting the minted-key pairing
# skew (see "Keys" below). Not for production.
taipan up --name demo --devkey

# Seed a small synthetic event stream (useful before any real traffic flows).
taipan demo --name demo

# Stop everything this environment started. Idempotent.
taipan down --name demo
```

## `~/.taipan` layout

```
~/.taipan/
  bin/                          cached built binaries + staleness markers
  events/                       one NDJSON file per service, shared across environments
    tokenfuse.ndjson
    wardryx.ndjson               (only if --with wardryx)
    demo.ndjson                  (only after `taipan demo`)
  environments/
    <name>.json                  descriptor — what other tools auto-discover
    <name>.pid.json               tracked PIDs, used and cleaned up by `taipan down`
    <name>.keys.json              dev bearer keys, mode 0600 — see "Keys" below
    <name>.wardryx-policy.yaml    demo Wardryx policy (only if --with wardryx)
    <name>.logs/<service>.log     stdout+stderr of each spawned process
    <name>.traces/gateway/        gateway's own Parquet trace dir
```

## The descriptor

`taipan up` writes `~/.taipan/environments/<name>.json` in the shape
consumers (the Genaryx console's auto-discovery) expect:

```json
{
  "name": "demo",
  "created_at": "2026-07-17T10:00:00Z",
  "host": "my-mac",
  "services": {
    "gateway": { "url": "http://127.0.0.1:4100", "mode": "enforce" },
    "cloud": { "url": "http://127.0.0.1:8080" },
    "wardryx": { "url": "http://127.0.0.1:8090" },
    "idryx": { "url": "http://127.0.0.1:8081" }
  },
  "events": {
    "dir": "/Users/you/.taipan/events",
    "files": { "tokenfuse": "tokenfuse.ndjson", "wardryx": "wardryx.ndjson" }
  },
  "keys": {
    "cloud_admin_ref": "taipan/demo/cloud_admin",
    "cloud_viewer_ref": "taipan/demo/cloud_viewer",
    "wardryx_admin_ref": "taipan/demo/wardryx_admin",
    "wardryx_viewer_ref": "taipan/demo/wardryx_viewer"
  },
  "unavailable": {},
  "logs_dir": "/Users/you/.taipan/environments/demo.logs"
}
```

`wardryx`/`idryx` entries and their key refs are present only when started via
`--with`. `unavailable` lists any `--with` service that failed to build or
start, keyed by service name, with a plain-text reason — `taipan up` degrades
gracefully rather than failing the whole environment over an optional piece,
and never omits a failure silently. `unavailable` and `logs_dir` are additive
beyond the documented shape; unknown fields are meant to be tolerated, the
same convention the agent-event envelope itself uses.

## Keys

Bearer keys (`key:org:role`, matching `tokenfuse-cloud`'s and Wardryx's own
key format) are minted fresh per environment and written to
`<name>.keys.json` (mode 0600), never into the descriptor. The descriptor
only carries a reference label (`taipan/<name>/<key-name>`) pointing at that
file's own key names — the same "value in a secret store, reference in the
discovered file" split the console's design calls for; a future
Keychain-backed connector replaces the local keyfile without changing the
descriptor shape. Cloud is additionally started with
`TOKENFUSE_CLOUD_ALLOW_DEVKEY=1` as a documented, dev-only fallback
credential alongside the minted keys.

### `--devkey`

With minted keys present, `tokenfuse-cloud`'s own key parser never activates
its `devkey` fallback (it only fires when the parsed key map is empty), so
pairing a device against a minted admin key over `/v1/pair/new` currently
401s even though reads with that same key work fine. `taipan up --devkey`
works around this: Cloud is started with an empty `TOKENFUSE_CLOUD_KEYS`
instead of minted keys (still with `TOKENFUSE_CLOUD_ALLOW_DEVKEY=1`), which
switches on the literal `devkey` bearer fallback, and `<name>.keys.json`
carries the literal string `"devkey"` under both the `cloud_admin` and
`cloud_viewer` labels. An auto-discovering console then reads and pairs with
a bearer Cloud genuinely accepts. The descriptor's `keys.cloud_admin_ref` /
`keys.cloud_viewer_ref` shape is unchanged; only the secret those refs
resolve to is different. This is a dev convenience for unblocking
console auto-pairing locally, never a production mode: without `--devkey`,
`up` behaves exactly as before (minted keys, devkey fallback unused because
the key map is never empty).

## Stopping cleanly

Every process `taipan up` starts is placed in its own process group at spawn
time (`setpgid(0, 0)`), and its PID is recorded in `<name>.pid.json`.
`taipan down` signals only PIDs it finds in that file, by process group —
never a PID discovered by scanning `ps`/`lsof`/`grep`. The gateway is
stopped with `SIGINT` specifically (its shutdown future is
`tokio::signal::ctrl_c()`, and that graceful path is what flushes its
buffered Parquet trace rows); every other service gets `SIGTERM`. Either way,
a process that ignores the first signal is escalated to `SIGKILL`. If
something is still alive after that, `down` reports it, leaves it in the
pidfile for a retry, and exits nonzero rather than pretending the teardown
was clean.

## Building

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
cargo test
```
