#!/usr/bin/env bash
# Fetches the two VM pieces the OCI runtime boots from (docs/one-pager-oci-runtime.md):
# the Firecracker release binary and the guest kernel Firecracker's CI
# publishes, at the versions and checksums crates/function-oci/Dockerfile
# pins. The Dockerfile is the one place those pins are written (Renovate
# moves the Firecracker ones there); this script reads them, so the
# Makefile, CI and the image agree by construction. A directory fetched at
# other pins is fetched again.
#
#   hack/oci-tools.sh <x86_64|aarch64> <directory>
# OCI_FIRECRACKER_VERSION (vX.Y.Z) fetches another release instead, checked
# against the checksum file that release publishes, and OCI_KERNEL_VERSION
# with OCI_KERNEL_SHA256 another guest kernel of the pinned artifact prefix:
# the release matrix's older legs.
#
# writes <directory>/firecracker and <directory>/vmlinux.
set -euo pipefail

arch=${1:?the architecture: x86_64 or aarch64}
dir=${2:?the output directory}
dockerfile="$(cd "$(dirname "$0")/.." && pwd)/crates/function-oci/Dockerfile"

arg() { sed -n "s/^ARG $1=//p" "$dockerfile" | head -n 1; }
checksum() { if command -v sha256sum >/dev/null; then sha256sum -c - >/dev/null; else shasum -a 256 -c - >/dev/null; fi; }

case "$arch" in
  x86_64) suffix=AMD64 ;;
  aarch64) suffix=ARM64 ;;
  *) echo "unsupported architecture $arch (x86_64 or aarch64)" >&2; exit 1 ;;
esac

fc_version=$(arg FIRECRACKER_VERSION)
fc_sha=$(arg "FIRECRACKER_SHA256_$suffix")
kernel_ci=$(arg KERNEL_CI)
kernel_version=$(arg KERNEL_VERSION)
kernel_sha=$(arg "KERNEL_SHA256_$suffix")
[ -n "$fc_version" ] && [ -n "$fc_sha" ] && [ -n "$kernel_ci" ] && [ -n "$kernel_version" ] && [ -n "$kernel_sha" ] \
  || { echo "cannot read the pins from $dockerfile" >&2; exit 1; }

if [ -n "${OCI_FIRECRACKER_VERSION:-}" ] && [ "$OCI_FIRECRACKER_VERSION" != "$fc_version" ]; then
  fc_version=$OCI_FIRECRACKER_VERSION
  fc_sha=$(curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/$fc_version/firecracker-$fc_version-$arch.tgz.sha256.txt" | awk '{print $1}')
  [ -n "$fc_sha" ] || { echo "cannot read the checksum of firecracker $fc_version" >&2; exit 1; }
fi
if [ -n "${OCI_KERNEL_VERSION:-}" ]; then
  kernel_version=$OCI_KERNEL_VERSION
  kernel_sha=${OCI_KERNEL_SHA256:?OCI_KERNEL_VERSION needs OCI_KERNEL_SHA256}
fi
pins="firecracker $fc_version kernel $kernel_ci $kernel_version $arch"
if [ -x "$dir/firecracker" ] && [ -f "$dir/vmlinux" ] && [ "$(cat "$dir/.pins" 2>/dev/null)" = "$pins" ]; then
  exit 0
fi

mkdir -p "$dir/release"
echo "fetching firecracker $fc_version ($arch)"
curl -fsSL -o "$dir/release/firecracker.tgz" \
  "https://github.com/firecracker-microvm/firecracker/releases/download/$fc_version/firecracker-$fc_version-$arch.tgz"
echo "$fc_sha  $dir/release/firecracker.tgz" | checksum
tar -xzf "$dir/release/firecracker.tgz" -C "$dir/release"
cp "$dir/release/release-$fc_version-$arch/firecracker-$fc_version-$arch" "$dir/firecracker"
chmod 0755 "$dir/firecracker"

echo "fetching the guest kernel $kernel_version ($kernel_ci, $arch)"
curl -fsSL -o "$dir/vmlinux" \
  "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/$kernel_ci/$arch/vmlinux-$kernel_version"
echo "$kernel_sha  $dir/vmlinux" | checksum
echo "$pins" > "$dir/.pins"
