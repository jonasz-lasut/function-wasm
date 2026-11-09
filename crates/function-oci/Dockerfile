# syntax=docker/dockerfile:1

# The function-oci runtime image (docs/one-pager-oci-runtime.md): the
# function-oci binary (the runtime over the OCI engine), the guest init
# built static for musl, and the microVM pieces the engine boots a VM from -
# the Firecracker binary from its release and the guest kernel Firecracker's
# CI publishes, both pinned by checksum. The pod needs /dev/kvm
# (docs/oci/install.md).
ARG RUST_VERSION=1
ARG FIRECRACKER_VERSION=v1.18.0
# The checksums of firecracker-${FIRECRACKER_VERSION}-<arch>.tgz. Renovate
# moves each with a version beside it: the amd64 one with the version line,
# the arm64 one with the marker over it, which states the version once more.
ARG FIRECRACKER_SHA256_AMD64=1ac8eec866dd5eea14efecf6323e22b23eea24054545338a5b34bc5bca6623cd
# renovate: firecracker v1.18.0 arm64
ARG FIRECRACKER_SHA256_ARM64=2a6130ff0338901ff9d7fbd73ce046c6b1c6123889dc84a34d678aa53bd0c0c2
# The kernel: firecracker-ci/<ci>/<arch>/vmlinux-<version> in Firecracker's
# artifact bucket (https://s3.amazonaws.com/spec.ccfc.min), the microvm
# kernel its CI builds from Amazon Linux (6.18 since Firecracker 1.16.1),
# under a dated prefix, with its sha256. The Makefile pins the same.
ARG KERNEL_CI=20261105-6ea6634994fa-0
ARG KERNEL_VERSION=6.18.56
ARG KERNEL_SHA256_AMD64=af348226aa2a25b846156150d5cdb5dae3bf2a022f1db176e3a2b9a3249817bf
ARG KERNEL_SHA256_ARM64=5e1990cd2f6e1c70196955b9405e32a5059f9f0a2c839478bc4f064a08f3a2ff

FROM --platform=${BUILDPLATFORM} rust:${RUST_VERSION} AS build
ARG BUILDARCH
ARG TARGETARCH

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends cmake musl-tools && \
    if [ "${BUILDARCH}" != "${TARGETARCH}" ]; then \
      case "${TARGETARCH}" in \
        arm64) pkgs="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
        amd64) pkgs="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
        *) echo "unsupported TARGETARCH ${TARGETARCH}" >&2; exit 1 ;; \
      esac; \
      apt-get install -y --no-install-recommends ${pkgs}; \
    fi

WORKDIR /fn

ARG GUESTFN_VERSION=
ENV GUESTFN_VERSION=${GUESTFN_VERSION}

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-gnu-gcc

# The function-oci binary, and the init (static, musl) beside it.
RUN --mount=target=.,rw \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/fn/target \
    case "${TARGETARCH}" in \
      arm64) target="aarch64-unknown-linux-gnu"; musl="aarch64-unknown-linux-musl"; export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc ;; \
      amd64) target="x86_64-unknown-linux-gnu"; musl="x86_64-unknown-linux-musl"; export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc ;; \
      *) echo "unsupported TARGETARCH ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    if [ "${BUILDARCH}" != "${TARGETARCH}" ]; then rustup target add "${target}"; fi; \
    rustup target add "${musl}"; \
    cargo build --release --locked -p function-oci --target "${target}" && \
    cp "target/${target}/release/function-oci" /function && \
    cargo build --release --locked -p guestfn-init --target "${musl}" && \
    cp "target/${musl}/release/guestfn-init" /guestfn-init

# Firecracker and the guest kernel, fetched at their pinned versions and
# held to their checksums.
FROM --platform=${BUILDPLATFORM} debian:bookworm-slim AS vmm
ARG TARGETARCH
ARG FIRECRACKER_VERSION
ARG FIRECRACKER_SHA256_AMD64
ARG FIRECRACKER_SHA256_ARM64
ARG KERNEL_CI
ARG KERNEL_VERSION
ARG KERNEL_SHA256_AMD64
ARG KERNEL_SHA256_ARM64
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates curl
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) arch=x86_64; fc_sha="${FIRECRACKER_SHA256_AMD64}"; kernel_sha="${KERNEL_SHA256_AMD64}" ;; \
      arm64) arch=aarch64; fc_sha="${FIRECRACKER_SHA256_ARM64}"; kernel_sha="${KERNEL_SHA256_ARM64}" ;; \
      *) echo "unsupported TARGETARCH ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    mkdir -p /out; \
    curl -fsSL -o /tmp/firecracker.tgz "https://github.com/firecracker-microvm/firecracker/releases/download/${FIRECRACKER_VERSION}/firecracker-${FIRECRACKER_VERSION}-${arch}.tgz"; \
    if [ -n "${fc_sha}" ]; then echo "${fc_sha}  /tmp/firecracker.tgz" | sha256sum -c -; fi; \
    tar -xzf /tmp/firecracker.tgz -C /tmp; \
    cp "/tmp/release-${FIRECRACKER_VERSION}-${arch}/firecracker-${FIRECRACKER_VERSION}-${arch}" /out/firecracker; \
    chmod 0755 /out/firecracker; \
    curl -fsSL -o /out/vmlinux "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/${KERNEL_CI}/${arch}/vmlinux-${KERNEL_VERSION}"; \
    if [ -n "${kernel_sha}" ]; then echo "${kernel_sha}  /out/vmlinux" | sha256sum -c -; fi

# The image: crates/function-wasm/Dockerfile's base (see there), the runtime, and
# the VM pieces under /usr/lib/guestfn, where the engine's flags default to.
FROM cgr.dev/chainguard/glibc-dynamic:latest@sha256:671a621016fb33812136dea6182b1db957996d3c8c618187aaf86abd9b14af53 AS image
WORKDIR /
COPY --from=build /function /function
COPY --from=build /guestfn-init /usr/lib/guestfn/init
COPY --from=vmm /out/firecracker /usr/lib/guestfn/firecracker
COPY --from=vmm /out/vmlinux /usr/lib/guestfn/vmlinux
EXPOSE 9443
USER nonroot:nonroot
ENTRYPOINT ["/function"]
