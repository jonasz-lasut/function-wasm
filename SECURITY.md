# Security policy

This policy is for anyone who depends on function-wasm or function-oci or
wants to report a vulnerability in either: how to report one, which
releases receive fixes and where the fixes for each kind of code in this
repository come from.

## Reporting a vulnerability

Report a vulnerability through GitHub's private reporting form, "Report a
vulnerability" under the repository's Security tab, rather than in a public
issue. The report is private to you and the maintainers until the advisory
is published.

## Supported releases

Through 0.x only the latest release line receives fixes; upgrade to the
latest release to get one. From 1.0.0 on, an older release line can be
patched on request.

## The runtime

Each function's `function` binary, its image, the workspace `Cargo.lock`
both binaries are built from, and the Crossplane package that embeds the
image: the code that runs in your cluster. function-oci's image also
contains a guest init built from the same workspace, the Firecracker binary
from Firecracker's release archive and the guest kernel that Firecracker's
CI builds. The maintainers fix a vulnerability here as a patch release of
the latest release line. Every pull request runs `cargo deny` over the dependency graph of the workspace (`deny.toml`, with every feature on), so a
published advisory against a pinned crate fails the merge, and the same
check runs every week against `main`. Renovate opens a pull request when an
advisory affects one of the workspace's crates.

The `grype-scan.yml` workflow scans each function's latest release image
every week and reports what it finds to GitHub code scanning. function-oci
has no release yet, and its scan starts with the first one. Grype reads
package metadata, which in these images is the Chainguard base's alone:
the C library, the GCC runtime libraries and the CA certificates. It
doesn't see the crates inside the Rust binaries, which `cargo deny` checks
instead, and it reports nothing for Firecracker or the guest kernel.
`crates/function-oci/Dockerfile` pins both by checksum. Renovate opens a
pull request for each Firecracker release, and the maintainers move the
kernel by hand in that pull request, to the kernel the release's CI built.
No scan in this repository alerts on a CVE in either.

## Scaffolded projects

The projects `guestfn init` writes, from the templates under
`crates/guestfn/templates/`. You build and publish these. Each release
refreshes their dependency pins, so a project scaffolded with the latest
`guestfn` starts current. Once written, the project is yours: `guestfn`
doesn't update it, so update its dependencies as you would in any other
project.

## Examples

The guests under `examples/` illustrate the guest languages. Four of them,
`function-wasm-rust`, `function-wasm-ts`, `function-wasm-go` and
`function-oci-go` (an image with its manifest layer), are published signed
under `ghcr.io/jonasz-lasut/guestfn/examples/` (`wasm-rust`, `wasm-ts`,
`wasm-go`, `oci-go`), so a Composition can
reference them. The runtime images contain no example code, and both
packages embed only the example manifests of `examples/function-wasm-go/example`.
Renovate refreshes the examples' dependencies in a monthly batch that merges
once the end-to-end suite passes, and its vulnerability pull requests against
them merge when their checks pass. The maintainers dismiss an alert against
an example when a sandboxed WebAssembly guest can't reach the code or no fix
exists.
