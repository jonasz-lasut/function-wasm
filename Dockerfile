# syntax=docker/dockerfile:1

# The Rust toolchain builds on the build platform and cross-compiles to the
# target: wasmtime (Cranelift) is pure Rust, and the only C in the graph
# (aws-lc, via sigstore's cosign support) builds with the cross gcc below.
ARG RUST_VERSION=1

FROM --platform=${BUILDPLATFORM} rust:${RUST_VERSION} AS build
ARG BUILDARCH
ARG TARGETARCH

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends cmake && \
    if [ "${BUILDARCH}" != "${TARGETARCH}" ]; then \
      case "${TARGETARCH}" in \
        arm64) pkgs="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
        amd64) pkgs="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
        *) echo "unsupported TARGETARCH ${TARGETARCH}" >&2; exit 1 ;; \
      esac; \
      apt-get install -y --no-install-recommends ${pkgs}; \
    fi

WORKDIR /fn

# The runtime's release version, stamped into the binary for minRuntime
# checks; a local build stays a development build (empty).
ARG FUNCTION_WASM_VERSION=
ENV FUNCTION_WASM_VERSION=${FUNCTION_WASM_VERSION}

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc

RUN --mount=target=.,rw \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/fn/target \
    case "${TARGETARCH}" in \
      arm64) target="aarch64-unknown-linux-gnu"; export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc ;; \
      amd64) target="x86_64-unknown-linux-gnu"; export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc ;; \
      *) echo "unsupported TARGETARCH ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    if [ "${BUILDARCH}" != "${TARGETARCH}" ]; then rustup target add "${target}"; fi; \
    cargo build --release --locked -p function-wasm --target "${target}" && \
    cp "target/${target}/release/function" /function

# Produce the Function image. The binary links glibc dynamically, so the
# base must ship glibc, libgcc_s and libstdc++: Chainguard's glibc-dynamic
# does (Wolfi glibc, rebuilt daily - it scanned clean where
# distroless/cc-debian13 carried a dozen won't-fix libc6 CVEs) with a
# nonroot user (65532), CA certificates and a writable /tmp for the caches.
# Renovate pins and bumps the digest.
FROM cgr.dev/chainguard/glibc-dynamic:latest@sha256:d49aa7837ef1ef8fae33917f94369294c6d49940d2f0b225beee65a3bb6747ed AS image
WORKDIR /
COPY --from=build /function /function
EXPOSE 9443
USER nonroot:nonroot
ENTRYPOINT ["/function"]
