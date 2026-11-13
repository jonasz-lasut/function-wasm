#!/usr/bin/env bash
# TEMPORARY, not for merge: runs the OCI engine's hand-run benchmark
# (crates/engine-oci/tests/bench.rs) twice over and prints every table.
# The ci.yml job of the same name sets the GUESTFN_* files.
#
#   hack/oci-bench.sh <output file>
set -euo pipefail

out=${1:?the file the tables are appended to}

bench() {
  cargo test --locked --release -p guestfn-engine-oci --features bench --test bench -- \
    --ignored --nocapture --test-threads=1 "$@" 2>&1 | tee -a bench.log |
    grep -E '^(\||###|[0-9]+ runs|The (caller|median)|failed|test result|SKIPPED)' | tee -a "$out"
}

bench packing_by_level

for pass in 1 2; do
  echo "## Pass $pass" | tee -a "$out"
  GUESTFN_BENCH_LABEL="pass $pass" bench a_run_by_phase
  GUESTFN_BENCH_LABEL="pass $pass" bench a_run_by_root_filesystem_packing
done
