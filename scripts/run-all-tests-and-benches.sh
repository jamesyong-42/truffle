#!/usr/bin/env bash
# run-all-tests-and-benches.sh — run every test and benchmark in RFC 019 Phase 1,
# with clear progress output, timing per phase, a summary table, and auto-open of
# all reports when done.
#
# Usage:
#   scripts/run-all-tests-and-benches.sh          # full run
#   scripts/run-all-tests-and-benches.sh --fast   # skip heavy macro benches (100M file, B2 concurrent, 500-RTT)
#   scripts/run-all-tests-and-benches.sh --no-open  # don't open reports at the end
#   scripts/run-all-tests-and-benches.sh --help
#
# Without TRUFFLE_TEST_AUTHKEY, real-network phases are skipped automatically.

set -uo pipefail

# ─── config ─────────────────────────────────────────────────────────────
FAST=0
OPEN_REPORTS=1

for arg in "$@"; do
  case "$arg" in
    --fast) FAST=1 ;;
    --no-open) OPEN_REPORTS=0 ;;
    -h|--help)
      sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown arg: $arg (try --help)" >&2
      exit 2
      ;;
  esac
done

# ─── colour / symbol helpers ────────────────────────────────────────────
if [ -t 1 ] && tput colors >/dev/null 2>&1; then
  BOLD=$'\033[1m'
  DIM=$'\033[2m'
  RED=$'\033[0;31m'
  GREEN=$'\033[0;32m'
  YELLOW=$'\033[0;33m'
  BLUE=$'\033[0;34m'
  CYAN=$'\033[0;36m'
  NC=$'\033[0m'
else
  BOLD='' DIM='' RED='' GREEN='' YELLOW='' BLUE='' CYAN='' NC=''
fi

OK="${GREEN}✔${NC}"
FAIL="${RED}✘${NC}"
SKIP="${YELLOW}↷${NC}"
ARROW="${CYAN}▸${NC}"

# ─── helpers ────────────────────────────────────────────────────────────
hr() { printf "${DIM}%s${NC}\n" "────────────────────────────────────────────────────────────────────"; }
h1() { printf "\n${BOLD}${BLUE}== %s ==${NC}\n" "$*"; }
h2() { printf "${BOLD}%s${NC}\n" "$*"; }
info() { printf "  ${ARROW} %s\n" "$*"; }
note() { printf "  ${DIM}%s${NC}\n" "$*"; }

# format_seconds <float_seconds> → e.g. "1m 23s" or "42.1s"
fmt_secs() {
  awk -v s="$1" 'BEGIN {
    if (s < 60) { printf "%.1fs", s }
    else if (s < 3600) { printf "%dm %ds", int(s/60), int(s)%60 }
    else { printf "%dh %dm", int(s/3600), int((s%3600)/60) }
  }'
}

# now_ns — nanoseconds since epoch, monotonic-ish
now_ns() {
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import time; print(int(time.time()*1e9))'
  else
    # fallback: second precision
    echo $(($(date +%s) * 1000000000))
  fi
}

# Tracked results, aligned arrays.
PHASE_NAMES=()
PHASE_STATUSES=()  # ok | fail | skip
PHASE_DURATIONS=() # formatted

record() {
  PHASE_NAMES+=("$1")
  PHASE_STATUSES+=("$2")
  PHASE_DURATIONS+=("$3")
}

# run_phase "<label>" <cmd...> — run a phase with timing + status
run_phase() {
  local label="$1"
  shift
  h2 "${ARROW} ${label}"
  note "$*"
  local t0
  t0=$(now_ns)
  if "$@"; then
    local t1
    t1=$(now_ns)
    local dur
    dur=$(awk -v a="$t0" -v b="$t1" 'BEGIN { print (b-a)/1e9 }')
    local pretty
    pretty=$(fmt_secs "$dur")
    printf "  ${OK} ${GREEN}ok${NC} ${DIM}(%s)${NC}\n\n" "$pretty"
    record "$label" "ok" "$pretty"
    return 0
  else
    local rc=$?
    local t1
    t1=$(now_ns)
    local dur
    dur=$(awk -v a="$t0" -v b="$t1" 'BEGIN { print (b-a)/1e9 }')
    local pretty
    pretty=$(fmt_secs "$dur")
    printf "  ${FAIL} ${RED}FAILED${NC} (exit %d, %s)\n\n" "$rc" "$pretty"
    record "$label" "fail" "$pretty"
    return $rc
  fi
}

mark_skipped() {
  printf "  ${SKIP} ${YELLOW}skipped${NC} — %s\n\n" "$2"
  record "$1" "skip" "—"
}

# ─── preflight ──────────────────────────────────────────────────────────
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

h1 "RFC 019 — full test + benchmark run"
info "Working dir: $(pwd)"
info "Mode: $([ "$FAST" = "1" ] && echo "fast (skip heavy macro benches)" || echo "full")"
info "Auto-open reports: $([ "$OPEN_REPORTS" = "1" ] && echo "yes" || echo "no")"

# Load .env so TRUFFLE_TEST_AUTHKEY flows into our subprocesses.
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
  info ".env loaded"
fi

HAVE_AUTHKEY=0
if [ -n "${TRUFFLE_TEST_AUTHKEY:-}" ] || [ -n "${TS_AUTHKEY:-}" ]; then
  HAVE_AUTHKEY=1
  info "auth key present → real-network phases will run"
else
  info "${YELLOW}no TRUFFLE_TEST_AUTHKEY${NC} → real-network phases will skip"
fi

TOTAL_T0=$(now_ns)

# ─── 0. build workspace (also triggers test-sidecar build.rs) ───────────
hr
run_phase "build workspace"        cargo build --workspace --quiet || :

# ─── 1. unit tests (no network) ─────────────────────────────────────────
hr
run_phase "unit tests"             cargo test --workspace --quiet || :

# ─── 2. real-network integration tests ──────────────────────────────────
hr
if [ "$HAVE_AUTHKEY" = "1" ]; then
  h2 "${ARROW} real-network integration tests"
  note "cargo test -p truffle-core --tests  (skip-gracefully helpers read \$TRUFFLE_TEST_AUTHKEY)"
  t0=$(now_ns)
  if cargo test -p truffle-core --tests --quiet; then
    t1=$(now_ns)
    pretty=$(fmt_secs "$(awk -v a="$t0" -v b="$t1" 'BEGIN { print (b-a)/1e9 }')")
    printf "  ${OK} ${GREEN}ok${NC} ${DIM}(%s)${NC}\n\n" "$pretty"
    record "real-network integration tests (16)" "ok" "$pretty"
  else
    t1=$(now_ns)
    pretty=$(fmt_secs "$(awk -v a="$t0" -v b="$t1" 'BEGIN { print (b-a)/1e9 }')")
    printf "  ${FAIL} ${RED}FAILED${NC} (%s)\n\n" "$pretty"
    record "real-network integration tests (16)" "fail" "$pretty"
  fi
else
  h2 "${ARROW} real-network integration tests"
  mark_skipped "real-network integration tests (16)" "TRUFFLE_TEST_AUTHKEY not set"
fi

# ─── 3. tier-1 micro benchmarks (criterion) ─────────────────────────────
hr
run_phase "tier-1 micro benchmarks (criterion)" \
    cargo bench -p truffle-core --quiet || :

# ─── 4. tier-2 macro benchmarks ─────────────────────────────────────────
hr
if [ "$HAVE_AUTHKEY" = "1" ]; then
  h2 "tier-2 macro benchmarks"

  run_phase "file-transfer 1M (5 iters)" \
      cargo run -p truffle-bench --release --quiet -- \
        file-transfer --size 1M --iterations 5 --warmup 1 || :

  if [ "$FAST" = "0" ]; then
    run_phase "file-transfer 10M (5 iters)" \
        cargo run -p truffle-bench --release --quiet -- \
          file-transfer --size 10M --iterations 5 --warmup 1 || :

    run_phase "file-transfer 100M (3 iters)" \
        cargo run -p truffle-bench --release --quiet -- \
          file-transfer --size 100M --iterations 3 --warmup 1 || :
  else
    mark_skipped "file-transfer 10M/100M" "--fast mode"
  fi

  run_phase "synced-store b1-sequential 1000 ops" \
      cargo run -p truffle-bench --release --quiet -- \
        synced-store --scenario b1-sequential --ops 1000 || :

  if [ "$FAST" = "0" ]; then
    run_phase "synced-store b2-concurrent 500 ops" \
        cargo run -p truffle-bench --release --quiet -- \
          synced-store --scenario b2-concurrent --ops 500 || :
  else
    mark_skipped "synced-store b2-concurrent" "--fast mode"
  fi

  COUNT=$([ "$FAST" = "1" ] && echo 100 || echo 500)
  run_phase "request-reply ${COUNT} round trips" \
      cargo run -p truffle-bench --release --quiet -- \
        request-reply --count "$COUNT" --payload-bytes 1024 || :
else
  h2 "tier-2 macro benchmarks"
  mark_skipped "all macro workloads" "TRUFFLE_TEST_AUTHKEY not set"
fi

# ─── summary ────────────────────────────────────────────────────────────
hr
TOTAL_T1=$(now_ns)
TOTAL=$(awk -v a="$TOTAL_T0" -v b="$TOTAL_T1" 'BEGIN { print (b-a)/1e9 }')

printf "\n${BOLD}Summary${NC}   (total $(fmt_secs "$TOTAL"))\n"
hr
printf "  %-48s  %-10s  %s\n" "Phase" "Status" "Time"
printf "  %-48s  %-10s  %s\n" "$(printf '%.0s-' {1..48})" "----------" "----"
ok_count=0 fail_count=0 skip_count=0
for i in "${!PHASE_NAMES[@]}"; do
  name="${PHASE_NAMES[$i]}"
  status="${PHASE_STATUSES[$i]}"
  dur="${PHASE_DURATIONS[$i]}"
  case "$status" in
    ok)   badge="${OK} ${GREEN}ok${NC}      "; ok_count=$((ok_count+1)) ;;
    fail) badge="${FAIL} ${RED}FAILED${NC}  "; fail_count=$((fail_count+1)) ;;
    skip) badge="${SKIP} ${YELLOW}skipped${NC} "; skip_count=$((skip_count+1)) ;;
  esac
  printf "  %-48s  %b  %s\n" "$name" "$badge" "$dur"
done
hr
printf "  ${OK} %d ok   ${FAIL} %d failed   ${SKIP} %d skipped\n\n" \
    "$ok_count" "$fail_count" "$skip_count"

# ─── outputs summary ────────────────────────────────────────────────────
CRIT_REPORT="target/criterion/report/index.html"
BENCH_DIR="target/bench-results"

h2 "Artefacts"
if [ -f "$CRIT_REPORT" ]; then
  info "Criterion HTML:    $CRIT_REPORT"
fi
if [ -d "$BENCH_DIR" ]; then
  count=$(ls -1 "$BENCH_DIR"/*.json 2>/dev/null | wc -l | tr -d ' ')
  info "Macro bench JSON:  $BENCH_DIR  ($count file(s))"
  # Print the latest result summaries inline
  if [ "$count" -gt 0 ]; then
    latest=$(ls -t "$BENCH_DIR"/*.json 2>/dev/null | head -5)
    echo
    note "Latest bench reports (most recent first):"
    for f in $latest; do
      label=$(basename "$f")
      case "$f" in
        *file-transfer*)
          mean=$(grep -o '"mean_throughput_mb_per_sec":[^,}]*' "$f" | head -1 | cut -d: -f2 | tr -d ' ')
          p50=$(grep -o '"p50_ms":[^,}]*' "$f" | head -1 | cut -d: -f2 | tr -d ' ')
          note "    $label  →  throughput≈${mean} MB/s  latency p50=${p50}ms"
          ;;
        *synced-store*)
          conv=$(grep -o '"convergence_ms":[^,}]*' "$f" | head -1 | cut -d: -f2 | tr -d ' ')
          ops=$(grep -o '"ops_per_sec_during_writes":[^,}]*' "$f" | head -1 | cut -d: -f2 | tr -d ' ')
          note "    $label  →  convergence=${conv}ms  writes≈${ops} ops/s"
          ;;
        *request-reply*)
          p50=$(grep -o '"p50":[^,}]*' "$f" | head -1 | cut -d: -f2 | tr -d ' ')
          p99=$(grep -o '"p99":[^,}]*' "$f" | head -1 | cut -d: -f2 | tr -d ' ')
          note "    $label  →  RTT p50=${p50}ms  p99=${p99}ms"
          ;;
        *)
          note "    $label"
          ;;
      esac
    done
  fi
fi
echo

# ─── open reports ───────────────────────────────────────────────────────
if [ "$OPEN_REPORTS" = "1" ]; then
  opener=""
  if command -v open >/dev/null 2>&1; then
    opener="open"
  elif command -v xdg-open >/dev/null 2>&1; then
    opener="xdg-open"
  fi

  if [ -n "$opener" ]; then
    h2 "Opening reports"
    if [ -f "$CRIT_REPORT" ]; then
      info "$opener $CRIT_REPORT"
      $opener "$CRIT_REPORT" >/dev/null 2>&1 || true
    fi
    if [ -d "$BENCH_DIR" ] && [ "$(ls -A "$BENCH_DIR" 2>/dev/null)" ]; then
      info "$opener $BENCH_DIR"
      $opener "$BENCH_DIR" >/dev/null 2>&1 || true
    fi
    echo
  else
    note "No 'open' or 'xdg-open' on PATH — skipping auto-open."
  fi
fi

# ─── final exit status ─────────────────────────────────────────────────
if [ "$fail_count" -gt 0 ]; then
  printf "${RED}${BOLD}%d phase(s) failed.${NC}\n" "$fail_count"
  exit 1
fi
printf "${GREEN}${BOLD}All phases clean.${NC}\n"
