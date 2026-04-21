# truffle — common developer tasks.
#
# Install `just` (https://github.com/casey/just) and run `just` to see the list.

default:
    @just --list

# Run only the fast unit tests (no real-Tailscale tests).
test:
    cargo test --workspace

# Run the real-network integration tests with extra logging.
# Requires TRUFFLE_TEST_AUTHKEY (in .env or shell env).
test-real:
    TRUFFLE_TEST_LOG="truffle_core=debug,integration_network=debug,integration_synced_store=debug,integration_request_reply=debug,integration_file_transfer=debug" \
        cargo test -p truffle-core --tests -- --nocapture

# Tier-1 criterion micro benchmarks.
bench-micro:
    cargo bench -p truffle-core

# Tier-2 macro benchmarks (all workloads). Requires TRUFFLE_TEST_AUTHKEY.
bench-macro:
    cargo run -p truffle-bench --release -- file-transfer --size 10M --iterations 5
    cargo run -p truffle-bench --release -- synced-store --scenario b1-sequential --ops 500
    cargo run -p truffle-bench --release -- request-reply --count 200

# Run a single macro benchmark workload. Example: `just bench-one file-transfer --size 100M`
bench-one *ARGS:
    cargo run -p truffle-bench --release -- {{ARGS}}

# Force-rebuild the test-sidecar binary (normally handled by build.rs).
build-test-sidecar:
    cd packages/sidecar-slim && go build -o ../../crates/truffle-core/tests/test-sidecar .

# Clean criterion + bench-results output.
bench-clean:
    rm -rf target/criterion target/bench-results

# Lint: clippy across the whole workspace, warnings treated as errors.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format everything.
fmt:
    cargo fmt --all

# Run the secret scanner once over the current staged diff (useful before `git commit`).
check-secrets:
    scripts/check-for-secrets.sh
