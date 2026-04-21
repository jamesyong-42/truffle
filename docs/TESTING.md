# Testing and Benchmarking — Local Phase

This guide covers **running truffle's test and benchmark suites on your own
machine**, against either mocks (no setup) or a real Tailscale network
(one-time setup). It implements Phase 1 of [RFC
019](rfcs/019-local-testing-and-benchmarking.md); CI automation is the next
phase.

---

## TL;DR

| You want to... | Command |
|---|---|
| Run all unit tests (no network) | `cargo test --workspace` |
| Run real-network integration tests | `TRUFFLE_TEST_AUTHKEY=tskey-... cargo test -p truffle-core` |
| Run micro benchmarks (no network) | `cargo bench -p truffle-core` |
| Run macro benchmarks against a real tailnet | `cargo run -p truffle-bench --release -- file-transfer` |

Without an auth key, real-network tests print `[skip] ...: TRUFFLE_TEST_AUTHKEY not set` and pass — you can contribute to 95% of the codebase without any Tailscale setup.

---

## One-Time Setup (for real-network tests)

You only need this if you want to run the integration tests that hit real
Tailscale — everything else works without it.

### 1. Create a Tailscale auth key

Go to <https://login.tailscale.com/admin/settings/keys> and generate an auth
key with:

- [x] **Reusable** — one key handles many test runs.
- [x] **Ephemeral** — test nodes auto-expire 30–60 min after shutdown.
- [x] **Tags: `tag:truffle-test`** — scopes the key so a stray leak can only
      mint test nodes, not production devices.
- [x] **Pre-approved** (only if your tailnet has device approval enabled).

The key looks like `tskey-auth-kABCDE1CNTRL-...`.

### 2. Add the tag to your tailnet ACL

If this is the first time using `tag:truffle-test`, your tailnet ACL needs
to declare it as an ownerless tag. Example snippet (add to your `tagOwners`
and `acls` sections):

```jsonc
{
  "tagOwners": {
    "tag:truffle-test": []  // or your admin group
  },
  "acls": [
    // Allow test nodes to reach each other — needed for the pair harness.
    {
      "action": "accept",
      "src":    ["tag:truffle-test"],
      "dst":    ["tag:truffle-test:*"]
    }
  ]
}
```

Apply the ACL, then the first test run will register the device.

### 3. Put the key in `.env`

At the repo root:

```bash
cp .env.example .env
# edit .env — set TRUFFLE_TEST_AUTHKEY=tskey-auth-...
```

`.env` is gitignored and loaded automatically by tests/benchmarks via
`dotenvy`. Don't commit it.

### 4. Install Go (for the test-sidecar build script)

The integration tests spawn a Go-based tsnet sidecar. Install Go 1.22+ from
<https://go.dev/dl/>. truffle-core's `build.rs` auto-detects Go and builds
the `test-sidecar` binary the first time you run integration tests.

If Go is missing, the build prints a clear warning and the tests fail with
instructions. Library builds stay green.

---

## Environment Variables

All variables read by tests and benchmarks. Defaults shown.

| Variable | Default | Purpose |
|---|---|---|
| `TRUFFLE_TEST_AUTHKEY` | _(unset → skip)_ | Tailscale auth key |
| `TS_AUTHKEY` | _(unset)_ | Legacy fallback; prints deprecation warning |
| `TRUFFLE_TEST_TAGS` | `tag:truffle-test` | Comma-separated tags to advertise |
| `TRUFFLE_TEST_EPHEMERAL` | `true` | Register nodes as ephemeral |
| `TRUFFLE_TEST_LOG` | `truffle_core=info` | `tracing_subscriber` filter |
| `TRUFFLE_TEST_STATE_ROOT` | `$TMPDIR` | Root for per-run node state dirs |

---

## Tests

### Structure

| Path | Network | Run by `cargo test` |
|---|---|---|
| `crates/*/src/**/*.rs` `#[cfg(test)]` blocks | Mocks / in-process | Yes — always |
| `crates/truffle-core/tests/integration_network.rs` | **Real Tailscale** (pair) | Yes (skip w/o auth) |
| `crates/truffle-core/tests/integration_synced_store.rs` | **Real Tailscale** (pair) | Yes (skip w/o auth) |
| `crates/truffle-core/tests/integration_request_reply.rs` | **Real Tailscale** (pair) | Yes (skip w/o auth) |
| `crates/truffle-core/tests/integration_file_transfer.rs` | **Real Tailscale** (pair) | Yes (skip w/o auth) |
| `crates/truffle-core/tests/integration_transport.rs` | In-process loopback (heavy) | No — still `#[ignore]` |

### Running

```bash
# Fast: unit + integration (integrations skip without a key)
cargo test --workspace

# Just the integration suite, verbose
TRUFFLE_TEST_LOG="truffle_core=debug,integration_synced_store=debug" \
    cargo test -p truffle-core --tests -- --nocapture

# Single test
cargo test -p truffle-core --test integration_synced_store \
    test_pair_converges_on_simple_write -- --nocapture
```

Or with `just`:

```bash
just test          # workspace unit + integration (skip)
just test-real     # integration with extra logging
```

### How the skip works

Every real-network test opens with:

```rust
let Some(authkey) = common::require_authkey("test_name") else { return; };
```

If `TRUFFLE_TEST_AUTHKEY` (or the legacy `TS_AUTHKEY`) is missing, the
helper prints a single `[skip]` line and the test returns as passing. No
`#[ignore]` attributes — the env var is the only switch.

### How the pair harness works

Each integration test spins up **two ephemeral tsnet nodes** on your
tailnet, waits for them to discover each other, and exercises real traffic
between them — no external EC2 or hand-configured peer required.

- Nodes are named `truffle-test-integ-<short-id>-{alpha,beta}` (Layer 3 pair) or
  `truffle-<app>-{alpha,beta}-<short-id>` (Node pair).
- Per-test `TempDir` for tsnet state — no cross-test contamination.
- `ephemeral=true` — Tailscale removes nodes ~30-60 min after the test ends.
- Child sidecar processes die via tokio's `kill_on_drop(true)`.

If the two nodes can't see each other within 45 s, the test fails with a
diagnostic listing what each node's `peers()` returned.

---

## Benchmarks

### Tier 1 — Micro benchmarks (criterion)

No Tailscale. Runs on every dev machine, every CI run.

```bash
cargo bench -p truffle-core                 # all three
cargo bench -p truffle-core --bench envelope
```

Files:
- `benches/envelope.rs` — JSON codec encode/decode throughput
- `benches/crdt_merge.rs` — Loro CRDT merge at 100 / 1k / 10k ops
- `benches/chunk_hash.rs` — sha2-256 throughput (full buffer + streamed)

Output lands in `target/criterion/`. Each bench run prints percentile
lines like:

```
envelope/decode/262144  time: [40.8 µs 40.9 µs 41.0 µs]
                        thrpt: [5.96 GiB/s 5.97 GiB/s 5.98 GiB/s]
```

### Tier 2 — Macro benchmarks (truffle-bench)

Real Tailscale pair. Requires `TRUFFLE_TEST_AUTHKEY`.

```bash
# File transfer — throughput for various payload sizes
cargo run -p truffle-bench --release -- file-transfer --size 10M --iterations 5

# SyncedStore convergence — B1 sequential (alpha writes, beta catches up)
cargo run -p truffle-bench --release -- synced-store \
    --scenario b1-sequential --ops 1000

# SyncedStore convergence — B2 concurrent (both write, both converge)
cargo run -p truffle-bench --release -- synced-store \
    --scenario b2-concurrent --ops 500

# Request/reply RTT — fires N round trips, reports p50/p95/p99/p999
cargo run -p truffle-bench --release -- request-reply \
    --count 500 --payload-bytes 1024
```

Or: `just bench-macro` runs the default matrix.

Each run writes a JSON report to `target/bench-results/{timestamp}-{workload}.json`
with per-iteration numbers, percentiles, and the current git SHA. Keep
these around to spot regressions on your own machine — absolute numbers
aren't comparable across machines.

---

## Pre-commit Hook (recommended)

```bash
git config core.hooksPath .githooks
```

Enables `.githooks/pre-commit`, which runs on every commit:

1. NAPI binding drift check
2. Workflow YAML sanity
3. Rustfmt on staged `.rs` files
4. Prettier on staged TypeScript files
5. `truffle` meta-crate doctest (if relevant files changed)
6. TypeScript build (if packages/ files changed)
7. **Secret scan** — blocks commits containing `tskey-` patterns

Skip once with `git commit --no-verify`. If the secret scanner flags a
legitimate doc example, add the file to `allowed_files` in
`scripts/check-for-secrets.sh`.

---

## Troubleshooting

**`test-sidecar binary not found`** — Go isn't installed or `go build`
failed. Install Go 1.22+ and re-run. Or manually: `cd packages/sidecar-slim
&& go build -o ../../crates/truffle-core/tests/test-sidecar .`

**`pair rendezvous timeout`** — Both nodes registered, but they couldn't
see each other within 45 s. Usual causes:
- Tailnet ACL doesn't allow `tag:truffle-test` → `tag:truffle-test`.
- Auth key was revoked, wasn't reusable, or expired.
- Tailscale control plane is slow (rare); re-run.

**`TRUFFLE_TEST_AUTHKEY is not set` (from `truffle-bench`)** — unlike tests,
the benchmark crate errors out rather than silently skipping. Set the key
or add it to `.env`.

**Benchmarks are wildly noisy** — expected on a shared dev machine. Close
other apps, disable background sync, and look at *relative* changes, not
absolute numbers. Dedicated hardware is a Phase-2 concern (RFC 020).

**Integration tests leak Tailscale devices** — they shouldn't:
`ephemeral=true` + short test lifetime means Tailscale removes them within
the hour. If you see stale devices in the admin panel, check that
`TRUFFLE_TEST_EPHEMERAL=true` (the default) hasn't been overridden.

**Tailscale free-plan ceiling** — 50 concurrent tagged devices. Each test
run makes 2 ephemeral devices. Running all integration tests serially uses
~20 at peak; parallelism uses more. If you hit the limit, wait ~30 min for
ephemeral cleanup, or tighten `TRUFFLE_TEST_LOG` and re-run.

---

## Design References

- [RFC 019 — Local Testing and Benchmarking Harness](rfcs/019-local-testing-and-benchmarking.md)
- [RFC 012 — Layered architecture](rfcs/012-layered-architecture-redesign.md)
- [RFC 016 — SyncedStore](rfcs/016-synced-store-and-request-reply.md)
- Future: **RFC 020 — CI Testing Harness** (OAuth, multi-runner matrix).
