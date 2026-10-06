#!/usr/bin/env bash
# Renders <example dir>/example/ with the function-wasm runtime from this
# repository serving that directory (module.type: Path, path: fn.wasm), the way the
# scaffold README describes. The guest must already be built to fn.wasm:
# each example's Makefile does that, whatever its toolchain.
#
# This script is the runtime's side of the render: it runs the runtime's own
# admission (function validate) over example/composition.yaml with every
# example/xr*.yaml, then serves the directory - with example/policy.cedar as
# the operator grant policy (--sandbox-policy-file), and example/fixtures/
# served over plain HTTP on 127.0.0.1:$FIXTURE_PORT (default 9480) for a
# module that fetches over the host's egress; the policy has to permit
# dialling loopback for it.
#
# Without --check it prints the render of example/xr.yaml, passing
# example/required-resources.yaml, observed-resources.yaml and
# function-credentials.yaml when they exist. With --check it runs the
# example's xprin suite, example/xprin.yaml
# (https://github.com/crossplane-contrib/xprin), which renders its cases and
# asserts what they compose - what CI runs: it proves the runtime loads the
# module, runs it over gRPC, Crossplane accepts the response, and the module
# does what the example says.
#
# The runtime is built from this repository unless FUNCTION_BIN names a
# prebuilt one (CI builds it once per run and hands it to every render job);
# xprin is XPRIN, else xprin on PATH.
#
# Usage: render.sh <example dir> [--check]
set -euo pipefail

here=$(cd "${1:?usage: render.sh <example dir> [--check]}" && pwd)
root=$(cd "$(dirname "$0")/.." && pwd)
check=false
[[ "${2:-}" == "--check" ]] && check=true
[[ -f "$here/fn.wasm" ]] || { echo "$here/fn.wasm not found; build the guest first" >&2; exit 1; }
example="$here/example"
fixture_port=${FIXTURE_PORT:-9480}
xprin=${XPRIN:-xprin}

listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
wait_for() {
  for _ in $(seq 1 40); do
    listening "$1" && return 0
    sleep 0.25
  done
  return 1
}

command -v crossplane >/dev/null || { echo "crossplane CLI not found; see https://docs.crossplane.io/latest/cli/" >&2; exit 1; }
if $check; then
  command -v "$xprin" >/dev/null || { echo "xprin not found; see https://github.com/crossplane-contrib/xprin" >&2; exit 1; }
  [[ -f "$example/xprin.yaml" ]] || { echo "$example/xprin.yaml not found" >&2; exit 1; }
fi
if listening 9443; then
  echo "something already listens on 127.0.0.1:9443 (the Development runtime target)" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'kill "${fn_pid:-}" "${fixtures_pid:-}" 2>/dev/null || true; rm -rf "$work"' EXIT

if [[ -n "${FUNCTION_BIN:-}" ]]; then
  echo "==> using the runtime at $FUNCTION_BIN" >&2
  cp "$FUNCTION_BIN" "$work/function"
else
  echo "==> building the runtime" >&2
  (cd "$root" && cargo build --release -p function-wasm >&2)
  cp "$root/target/release/function" "$work/function"
fi

runtime_flags=(--module-dir="$here")
[[ -f "$example/policy.cedar" ]] && runtime_flags+=(--sandbox-policy-file="$example/policy.cedar")

# The runtime's own admission over the example Composition, offline, before
# anything is served: the same flags the runtime is started with below, and
# --resolve reads fn.wasm's ABI the way the runtime will. Each XR goes in, so
# a module the XR chooses (module.from) is resolved as well.
for xr in "$example"/xr*.yaml; do
  echo "==> function validate ($(basename "$xr"))" >&2
  "$work/function" validate "$example/composition.yaml" "${runtime_flags[@]}" --xr="$xr" --resolve >&2
done

if [[ -d "$example/fixtures" ]]; then
  command -v python3 >/dev/null || { echo "python3 not found; it serves $example/fixtures" >&2; exit 1; }
  if listening "$fixture_port"; then
    echo "something already listens on 127.0.0.1:$fixture_port (the fixture server)" >&2
    exit 1
  fi
  echo "==> serving $example/fixtures on 127.0.0.1:$fixture_port" >&2
  python3 -m http.server "$fixture_port" --bind 127.0.0.1 --directory "$example/fixtures" >"$work/fixtures.log" 2>&1 &
  fixtures_pid=$!
  wait_for "$fixture_port" || { echo "fixture server did not start:" >&2; cat "$work/fixtures.log" >&2; exit 1; }
fi

echo "==> starting the runtime" >&2
"$work/function" --insecure "${runtime_flags[@]}" >"$work/function.log" 2>&1 &
fn_pid=$!
wait_for 9443 || { echo "runtime did not start:" >&2; cat "$work/function.log" >&2; exit 1; }

if $check; then
  echo "==> xprin test example/xprin.yaml" >&2
  if ! "$xprin" test -v --show-render --show-validate --show-assertions "$example/xprin.yaml"; then
    echo "--- runtime log ---" >&2
    cat "$work/function.log" >&2
    exit 1
  fi
  exit 0
fi

args=()
for input in required-resources observed-resources function-credentials; do
  [[ -f "$example/$input.yaml" ]] && args+=(--"$input"="$example/$input.yaml")
done
echo "==> crossplane composition render" >&2
(cd "$here" && crossplane composition render example/xr.yaml example/composition.yaml example/functions.yaml \
  --include-function-results ${args[@]+"${args[@]}"})
