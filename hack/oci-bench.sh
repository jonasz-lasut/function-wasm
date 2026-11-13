#!/usr/bin/env bash
# TEMPORARY, not for merge: runs the OCI engine's hand-run benchmark
# (crates/engine-oci/tests/bench.rs) for the measurement pull request, twice
# over, with the throwaway latency experiments beside the baseline, and
# prints every table. The ci.yml job of the same name sets the GUESTFN_*
# files; INIT_FAST and INIT_TRACE are the init built with its experiment
# features.
#
#   hack/oci-bench.sh <output file>
set -euo pipefail

out=${1:?the file the tables are appended to}
init=${GUESTFN_INIT:?}
init_fast=${INIT_FAST:?}
init_trace=${INIT_TRACE:?}

bench() {
  cargo test --locked --release -p guestfn-engine-oci --features bench --test bench -- \
    --ignored --nocapture --test-threads=1 "$@" 2>&1 | tee -a bench.log |
    grep -E '^(\||###|[0-9]+ runs|failed|test result|SKIPPED)' | tee -a "$out"
}

for pass in 1 2; do
  echo "## Pass $pass" | tee -a "$out"
  GUESTFN_BENCH_LABEL="baseline, pass $pass" bench a_run_by_phase
  GUESTFN_BENCH_LABEL="host polls at 50 us, pass $pass" GUESTFN_EXP_POLL_US=50 \
    bench a_run_by_phase
  GUESTFN_BENCH_LABEL="host polls at 50 us and init polls at 100 us, pass $pass" \
    GUESTFN_EXP_POLL_US=50 GUESTFN_INIT=$init_fast bench a_run_by_phase
  GUESTFN_BENCH_LABEL="both polls short and no wait for the exit, pass $pass" \
    GUESTFN_EXP_POLL_US=50 GUESTFN_EXP_NO_FINISH_WAIT=1 GUESTFN_INIT=$init_fast \
    bench a_run_by_phase
  GUESTFN_BENCH_LABEL="no wait for the exit alone, pass $pass" \
    GUESTFN_EXP_NO_FINISH_WAIT=1 bench a_run_by_phase
  GUESTFN_BENCH_LABEL="pass $pass" bench a_run_by_memory
  GUESTFN_BENCH_LABEL="pass $pass" bench a_run_by_root_filesystem_packing
done

# The init's own moments, from the serial console of a few runs.
echo "## The init's trace" | tee -a "$out"
GUESTFN_BENCH_RUNS=3 GUESTFN_VM_CONSOLE=1 GUESTFN_INIT=$init_trace \
  cargo test --locked --release -p guestfn-engine-oci --features bench --test bench -- \
  --ignored --nocapture --test-threads=1 a_run_by_phase 2>&1 | tee -a bench.log |
  grep -E '^(###|guestfn-init: (trace|resumed|parked)|\[ *[0-9.]+\].*(squashfs|Run /init|Freeing|VFS))' |
  sed 's/^/    /' | tee -a "$out"
