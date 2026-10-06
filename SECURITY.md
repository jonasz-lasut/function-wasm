# Security policy

This policy is for people who run the function-wasm runtime and people who
build guests from `guestfn init`. It covers vulnerabilities in the
dependencies of the three kinds of code in this repository and says where
the fixes for each come from.

## The runtime

The `function` binary, its image, the workspace `Cargo.lock` it is built
from, and the Crossplane package that embeds the image. This is the code
that runs in your cluster, so a vulnerability here is remediated promptly:
the fix ships as a patch release of the latest release line. Older lines
are not patched; upgrade to the latest release to get a fix. The
`grype-scan.yml` workflow scans the image of the latest release every week
and reports what it finds to GitHub code scanning, and Renovate opens a
pull request when an advisory affects one of the workspace's crates.

## Scaffolded projects

The projects `guestfn init` writes, from the templates under
`crates/guestfn/templates/`. You build and ship these, so their dependency
pins are refreshed at every release: a project scaffolded with the latest
`guestfn` starts current. Once written, the project is yours. `guestfn`
does not update it, so keep its dependencies current as you would in any
other project.

## Examples

The guests under `examples/` illustrate the guest languages and are never
shipped: the image holds only the runtime binary, and the package embeds
only the example manifests of `examples/hello-go/example`. Renovate
refreshes their dependencies in a monthly batch, which merges once the
end-to-end suite (`/e2e`) passes. An alert against an example that a
sandboxed WebAssembly guest cannot reach, or that has no fix, is dismissed
with a reason that points to this policy. Renovate's vulnerability pull
requests against the examples may be merged when their checks pass, but
are not chased between batches.
