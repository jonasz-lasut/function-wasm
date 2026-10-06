#!/usr/bin/env bash
# The OCI path end to end at the render tier (issue #112): a module pushed to a
# registry behind basic auth, signed with cosign, chosen by the composite
# resource through module.from, pulled by digest with the step credential the
# WebApp names, fenced by the compositionPolicy's pullModule, and verified
# against the operator policy's requireSignature. examples/pdb-addon is the
# module under test.
#
# This script sets the stage: a throwaway registry, the pushes and the
# signature, the XRs with the pushed references filled in, and the runtime.
# What is asserted lives in data: xprin.yaml for the renders, and the expected
# verdicts of function validate's JSON for the refusals (a refusal is a fatal
# result, which fails a render).
#
# Needs docker, the crossplane CLI (composition render), cosign, jq, xprin
# (XPRIN, else on PATH), guestfn (GUESTFN, else on PATH) and Go to build the
# add-on when examples/pdb-addon/fn.wasm is missing. The runtime is built from
# this repository unless FUNCTION_BIN names a prebuilt one.
#
# Usage: test/e2e/oci/run.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
port=5005
registry="localhost:${port}"
container=function-wasm-e2e-oci-registry
xprin=${XPRIN:-xprin}
guestfn=${GUESTFN:-guestfn}
addon="$root/examples/pdb-addon"

for tool in docker crossplane cosign jq "$xprin" "$guestfn"; do
  command -v "$tool" >/dev/null || { echo "$tool not found" >&2; exit 1; }
done

listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
wait_for() {
  for _ in $(seq 1 40); do
    listening "$1" && return 0
    sleep 0.25
  done
  return 1
}
for p in 9443 "$port"; do
  if listening "$p"; then
    echo "something already listens on 127.0.0.1:$p" >&2
    exit 1
  fi
done

work=$(mktemp -d)
trap 'kill "${fn_pid:-}" 2>/dev/null || true; docker stop "$container" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

if [[ -n "${FUNCTION_BIN:-}" ]]; then
  cp "$FUNCTION_BIN" "$work/function"
else
  echo "==> building the runtime" >&2
  (cd "$root" && cargo build --release -p function-wasm >&2)
  cp "$root/target/release/function" "$work/function"
fi
if [[ ! -f "$addon/fn.wasm" ]]; then
  echo "==> building examples/pdb-addon" >&2
  GUESTFN="$guestfn" make -C "$addon" build >&2
fi

echo "==> a registry behind basic auth on $registry" >&2
docker run -d --rm --name "$container" -p "127.0.0.1:${port}:5000" \
  -v "$here/htpasswd:/auth/htpasswd:ro" \
  -e REGISTRY_AUTH=htpasswd -e REGISTRY_AUTH_HTPASSWD_REALM=e2e -e REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd \
  registry:3 >/dev/null
wait_for "$port" || { echo "the registry did not start" >&2; exit 1; }

# A Docker config of the run's own: guestfn, cosign and function validate log
# in through it, and nothing touches the caller's.
export DOCKER_CONFIG="$work/docker"
mkdir -p "$DOCKER_CONFIG" "$work/no-docker"
cosign login "$registry" -u e2e -p e2e-password >/dev/null
(cd "$work" && COSIGN_PASSWORD='' cosign generate-key-pair >/dev/null)

push() {
  local repository=$1
  shift
  "$guestfn" push --file "$addon/fn.wasm" --manifest "$addon/wasmfn.yaml" "$@" "$registry/$repository:v0.1.0" \
    | sed -nE 's/^Pushed .*@(sha256:[0-9a-f]{64})$/\1/p'
}
echo "==> pushing the add-on" >&2
signed=$(push team-a/pdb)
# A key-based signature, recorded nowhere but the registry: no Rekor upload.
COSIGN_PASSWORD='' cosign sign --key "$work/cosign.key" --use-signing-config=false --tlog-upload=false \
  --allow-http-registry --yes "$registry/team-a/pdb@$signed" >/dev/null
# Its own content, so its digest is not the signed one: a signature covers a
# digest, wherever the artifact is pushed.
unsigned=$(push team-a/pdb-unsigned --module-version 0.1.0-unsigned)
outside=$(push elsewhere/pdb)
for d in "$signed" "$unsigned" "$outside"; do
  [[ -n "$d" ]] || { echo "guestfn push printed no digest" >&2; exit 1; }
done

mkdir -p "$work/scenario"
cp "$here"/{composition,functions,function-credentials,xprin,xr-none}.yaml "$work/scenario/"
for case in signed unsigned outside; do
  case $case in
    signed) ref="$registry/team-a/pdb@$signed" ;;
    unsigned) ref="$registry/team-a/pdb-unsigned@$unsigned" ;;
    outside) ref="$registry/elsewhere/pdb@$outside" ;;
  esac
  sed "s|@REF_$(tr '[:lower:]' '[:upper:]' <<<"$case")@|$ref|" "$here/xr-$case.yaml.tmpl" > "$work/scenario/xr-$case.yaml"
done

runtime_flags=(--cosign-key="$work/cosign.pub" --sandbox-policy-file="$here/policy.cedar")

# The runtime's own admission, offline: each case's verdict for the add-on
# step, as function validate's JSON states it.
echo "==> function validate" >&2
fail=false
expect() {
  local xr=$1 filter=$2 verdict
  verdict=$("$work/function" validate "$work/scenario/composition.yaml" --xr "$work/scenario/$xr" \
    --resolve --output json "${runtime_flags[@]}" | jq -c 'select(.step == "add-on")' || true)
  if jq -e --arg signed "$signed" "$filter" <<<"$verdict" >/dev/null; then
    echo "    ok: $xr" >&2
  else
    echo "    FAIL: $xr: $verdict" >&2
    fail=true
  fi
}
expect xr-signed.yaml '.status == "ok" and .resolved.digest == $signed'
expect xr-outside.yaml '.status == "refused" and (.message | startswith("cannot resolve module: module.from:")) and (.message | contains("compositionPolicy"))'
expect xr-unsigned.yaml '.status == "refused" and (.message | startswith("cannot verify module")) and (.message | contains("carries no cosign signature"))'
$fail && exit 1

# A Docker config holding nothing: the runtime's pull can only succeed through
# the step credential the WebApp names.
echo "==> starting the runtime" >&2
DOCKER_CONFIG="$work/no-docker" "$work/function" --insecure "${runtime_flags[@]}" >"$work/function.log" 2>&1 &
fn_pid=$!
wait_for 9443 || { echo "runtime did not start:" >&2; cat "$work/function.log" >&2; exit 1; }

echo "==> xprin test" >&2
if ! (cd "$work/scenario" && "$xprin" test -v --show-render --show-assertions xprin.yaml); then
  echo "--- runtime log ---" >&2
  cat "$work/function.log" >&2
  exit 1
fi
