# Security policy

This policy is for anyone who depends on function-wasm or wants to report a
vulnerability in it: how to report one, which releases receive fixes and
where the fixes for each kind of code in this repository come from.

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

The `function` binary, its image, the workspace `Cargo.lock` it's built
from, and the Crossplane package that embeds the image: the code that runs
in your cluster. The maintainers fix a vulnerability here as a patch release
of the latest release line. The `grype-scan.yml` workflow scans the image of
the latest release every week and reports what it finds to GitHub code
scanning, and Renovate opens a pull request when an advisory affects one of
the workspace's crates.

## Scaffolded projects

The projects `guestfn init` writes, from the templates under
`crates/guestfn/templates/`. You build and publish these. Each release
refreshes their dependency pins, so a project scaffolded with the latest
`guestfn` starts current. Once written, the project is yours: `guestfn`
doesn't update it, so update its dependencies as you would in any other
project.

## Examples

The guests under `examples/` illustrate the guest languages. Three of them,
`cloudflare-origin`, `policy-gate` and `pdb-addon`, are published as signed
OCI artifacts under `ghcr.io/jonasz-lasut/wasmfn/examples/`, so a Composition
can reference them. The runtime image contains only the runtime binary, and
the package embeds only the example manifests of `examples/pdb-addon/example`.
Renovate refreshes the examples' dependencies in a monthly batch that merges
once the end-to-end suite passes, and its vulnerability pull requests against
them merge when their checks pass. The maintainers dismiss an alert against
an example when a sandboxed WebAssembly guest can't reach the code or no fix
exists.
