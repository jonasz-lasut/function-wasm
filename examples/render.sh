#!/usr/bin/env bash
# Renders <example dir>/example/ with the function-wasm runtime from this
# repository serving that directory (module.type: Path, path: fn.wasm), the way the
# scaffold README describes. The guest must already be built to fn.wasm —
# each example's Makefile does that, whatever its toolchain. With --check the
# rendered output is asserted instead of printed, which is what CI runs: it
# proves the runtime loads the module, runs it over gRPC, and Crossplane
# accepts the response.
#
# Usage: render.sh <example dir> [--check]
set -euo pipefail

here=$(cd "${1:?usage: render.sh <example dir> [--check]}" && pwd)
root=$(cd "$(dirname "$0")/.." && pwd)
check=false
[[ "${2:-}" == "--check" ]] && check=true
[[ -f "$here/fn.wasm" ]] || { echo "$here/fn.wasm not found; build the guest first" >&2; exit 1; }

listening() { (exec 3<>/dev/tcp/127.0.0.1/9443) 2>/dev/null; }

command -v crossplane >/dev/null || { echo "crossplane CLI not found; see https://docs.crossplane.io/latest/cli/" >&2; exit 1; }
if listening; then
  echo "something already listens on 127.0.0.1:9443 (the Development runtime target)" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'kill "${fn_pid:-}" 2>/dev/null || true; rm -rf "$work"' EXIT

echo "==> building the runtime" >&2
(cd "$root" && go build -o "$work/function" ./cmd/function)

# The runtime's own admission over the example Composition, offline, before
# anything is served: the same flags the runtime is started with below, and
# --resolve reads fn.wasm's ABI the way the runtime will.
echo "==> function validate" >&2
"$work/function" validate "$here/example/composition.yaml" --module-dir="$here" --resolve >&2

echo "==> starting the runtime" >&2
"$work/function" --insecure --module-dir="$here" >"$work/function.log" 2>&1 &
fn_pid=$!
for _ in $(seq 1 40); do
  listening && break
  sleep 0.25
done
listening || { echo "runtime did not start:" >&2; cat "$work/function.log" >&2; exit 1; }

echo "==> crossplane render" >&2
out=$(cd "$here" && crossplane render example/xr.yaml example/composition.yaml example/functions.yaml --include-function-results)

if ! $check; then
  printf '%s\n' "$out"
  exit 0
fi

fail=false
for want in "kind: ConfigMap" "greeting: hi example-xr" "greeted example-xr" "type: FunctionSuccess"; do
  if ! grep -qF -- "$want" <<<"$out"; then
    echo "render output lacks: $want" >&2
    fail=true
  fi
done
if grep -q "SEVERITY_FATAL\|severity: Fatal" <<<"$out"; then
  echo "render output contains a fatal result" >&2
  fail=true
fi
if $fail; then
  printf '%s\n' "$out" >&2
  echo "--- runtime log ---" >&2
  cat "$work/function.log" >&2
  exit 1
fi
echo "render OK: ConfigMap greeting composed by $here/fn.wasm" >&2
