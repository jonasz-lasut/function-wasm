# syntax=docker/dockerfile:1

# go.mod is the source of truth for the Go version: CI passes the version it
# declares as GO_VERSION. A local build defaults to the latest Go 1.x image, and
# Go's toolchain selection still guarantees at least the version go.mod requires.
ARG GO_VERSION=1

# Setup the base environment. wasmtime-go is a CGo binding to a prebuilt static
# libwasmtime, so the build needs a C toolchain — a cross one when the build
# and target architectures differ (docker buildx on a laptop, or CI building
# arm64 on amd64).
FROM --platform=${BUILDPLATFORM} golang:${GO_VERSION} AS base
ARG BUILDARCH
ARG TARGETARCH
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    if [ "${BUILDARCH}" != "${TARGETARCH}" ]; then \
      case "${TARGETARCH}" in \
        arm64) pkgs="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
        amd64) pkgs="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
        *) echo "unsupported TARGETARCH ${TARGETARCH}" >&2; exit 1 ;; \
      esac; \
      apt-get update && apt-get install -y --no-install-recommends ${pkgs}; \
    fi

WORKDIR /fn
ENV CGO_ENABLED=1

COPY go.mod go.sum ./
RUN --mount=type=cache,target=/go/pkg/mod go mod download

# Build the Function.
FROM base AS build
ARG TARGETOS
ARG TARGETARCH
ARG BUILDARCH
RUN --mount=target=. \
    --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    if [ "${BUILDARCH}" != "${TARGETARCH}" ]; then \
      case "${TARGETARCH}" in \
        arm64) export CC=aarch64-linux-gnu-gcc ;; \
        amd64) export CC=x86_64-linux-gnu-gcc ;; \
      esac; \
    fi; \
    GOOS=${TARGETOS} GOARCH=${TARGETARCH} go build -trimpath -ldflags "-s -w" -o /function ./cmd/function

# Produce the Function image. The binary links glibc dynamically (wasmtime's
# static library needs libm, libdl, libpthread and libgcc_s), so the base
# must ship glibc, libgcc_s and libstdc++: Chainguard's glibc-dynamic does
# (Wolfi glibc, rebuilt daily — it scanned clean where distroless/cc-debian13
# carried a dozen won't-fix libc6 CVEs) with a nonroot user (65532), CA
# certificates and a writable /tmp for the caches. Its glibc must not be older
# than the one golang:${GO_VERSION} builds against (Debian trixie, 2.41);
# Wolfi tracks upstream closely, and Renovate pins and bumps the digest.
FROM cgr.dev/chainguard/glibc-dynamic:latest@sha256:df4e22a4b5dcd8e15a51fe9b04e16717d411dd9f4fe4b3844c1bf425b14be303 AS image
WORKDIR /
COPY --from=build /function /function
EXPOSE 9443
USER nonroot:nonroot
ENTRYPOINT ["/function"]
