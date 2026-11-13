#!/usr/bin/env bash
# Downloads one file and holds it to a pinned sha256 before anything runs
# it. The release workflows fetch the tools that hold registry credentials
# through it: a version moves together with its checksum, by hand.
#   hack/fetch-verified.sh <url> <sha256> <destination>
set -euo pipefail

url=${1:?the url to download}
want=${2:?the sha256 the file must have}
dest=${3:?where to put the file}

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL --retry 3 -o "$tmp" "$url"
got=$(sha256sum "$tmp" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
  echo "$url has sha256 $got, and $want is pinned: not installing it" >&2
  exit 1
fi
mkdir -p "$(dirname "$dest")"
install -m 0755 "$tmp" "$dest"
