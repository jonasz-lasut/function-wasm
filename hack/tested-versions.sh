#!/usr/bin/env bash
# Rewrites the release row of one function's tested-versions table in README.md from
# the record the release matrix writes (its tested-versions artifact). The
# release skills run it once a function's release is out.
#   hack/tested-versions.sh <function-wasm|function-oci> <vX.Y.Z> <tested-versions.json>
set -euo pipefail

function=${1:?the function: function-wasm or function-oci}
version=${2:?the released version: vX.Y.Z}
record=${3:?the release matrix record: tested-versions.json}
readme="$(cd "$(dirname "$0")/.." && pwd)/README.md"

crossplane=$(jq -r '[.crossplane[] | (split(".")[0:2] | join(".")) + " (v" + . + ")"] | join(", ")' "$record")
case "$function" in
  function-wasm) row="| $version | $crossplane |" ;;
  function-oci)
    vms=$(jq -r '[.vms[] | .firecracker + " × " + .kernel] | join(", ")' "$record")
    row="| $version | $crossplane | $vms |"
    ;;
  *) echo "unknown function $function (function-wasm or function-oci)" >&2; exit 1 ;;
esac

awk -v start="<!-- tested-versions:$function:start -->" -v end="<!-- tested-versions:$function:end -->" -v row="$row" '
  index($0, start) { inside = 1 }
  index($0, end) { inside = 0 }
  inside && /^\| v[0-9]/ { print row; found = 1; next }
  { print }
  END { if (!found) exit 1 }
' "$readme" > "$readme.tmp" || { rm -f "$readme.tmp"; echo "README.md has no tested-versions row for $function" >&2; exit 1; }
mv "$readme.tmp" "$readme"
