---
name: remediate-cves
description: Use when asked to remediate CVEs, security vulnerabilities, or GitHub code-scanning/Grype findings against a function-wasm or function-oci release (a Firecracker or guest-kernel advisory included) - e.g. "fix the CVEs from grype", "patch the vulnerabilities on the latest release", a security alert on a release tag, or a request to cut a security patch release.
---

# Remediating CVEs

## Overview

`.github/workflows/grype-scan.yml` runs weekly (Mondays 05:00 UTC) and scans
each function's latest release image with Grype, one matrix leg per function,
uploading the SARIF results to GitHub code scanning against
`refs/tags/<that tag>` under the leg's own category.

Each leg takes its function's newest `<function>-vX.Y.Z` GitHub release, scans
`ghcr.io/jonasz-lasut/<function>:vX.Y.Z` and files the results under that git
tag. A function with no release ends green with a notice.

Grype reads the package metadata of the Chainguard base both images share
(Wolfi's glibc, libgcc, libstdc++, CA certificates). It sees no crate inside
the `function` binary or function-oci's init (built without `cargo auditable`;
`cargo deny` covers them through `Cargo.lock`), and nothing in the Firecracker
binary or the guest kernel `vmlinux` in function-oci's image: a CVE in those
two arrives as an advisory or a Renovate pull request, never as an alert.

Remediating a CVE found this way means patching the released version's branch
and shipping a new patch release end to end: branch → fix → tag → release →
publish → sign. What ships is decided by the completeness gate in step 4.

## When to use

- Asked to remediate/fix CVEs, vulnerabilities, GHSA advisories, or grype /
  code-scanning findings tied to a release of this function.
- **Not** for routine dependency bumps with no security alert behind them
  (that's ordinary `chore(deps)` / Renovate work).
- A patch release of the `wasmtime` crates **is** in scope: see step 3. A
  new wasmtime major is a minor release from `main` (`/cut-release`), never
  a patch.
- A Firecracker or guest-kernel advisory against function-oci's image **is**
  in scope, though no scan reports one: see step 3.

## Procedure

### 1. Identify the target release branch

```bash
FUNCTION=function-wasm   # or function-oci: the one the finding is against
TAG=$(gh release list --limit 200 --json tagName -q '.[].tagName' \
  | grep -E "^${FUNCTION}-v[0-9]+\.[0-9]+\.[0-9]+$" | sort -V | tail -n 1)   # e.g. function-wasm-v1.0.0
VERSION=${TAG#"$FUNCTION-"}                                     # v1.0.0
BRANCH="release-$FUNCTION-$(echo "$VERSION" | sed -E 's/^v([0-9]+)\.([0-9]+)\..*/\1.\2/')"  # release-function-wasm-1.0
git fetch origin
git checkout "$BRANCH" 2>/dev/null || git checkout -b "$BRANCH" "$TAG"
```

Never use `gh release view` with no tag: the repository's latest release can
be another component's. If `origin/$BRANCH` doesn't exist yet, it must be cut
from the release **tag**, never from `main` HEAD: a security patch must not
carry unreleased changes.

The function's latest release is the default target. Every release branch is
kept, and from a function's v1.0.0 on an older line can be patched when the
user asks for it: set `TAG` to that line's latest tag
(`gh release list`) and derive `VERSION` and `BRANCH` from it the same way.
Before a function's first release there is nothing to patch: the fix lands on
`main` like any change. A fix that patches both functions is one run of this
procedure per function.

### 1b. Check the workflows the branch carries

A release branch runs its own workflow files, as they were at the cut: a
push to it runs the branch's `ci.yml` and `release-matrix-e2e.yml`, and
`Publish Function`, dispatched from the tag in step 8, runs the tag's
`publish-function.yml`. Nothing merged to `main`'s workflows since the cut
is on the branch.

```bash
git diff --stat origin/main "$BRANCH" -- .github/workflows hack
```

What differs falls into two groups, handled differently:

- **The publishing path** (`publish-function.yml` and the
  `hack/fetch-verified.sh` it calls) is what guards the release itself: the
  `guard` job, the CLI downloads held to a checksum, the job timeouts. If
  the branch's copy differs, bring `main`'s onto the branch before the fix,
  as a commit of its own:

  ```bash
  git checkout origin/main -- .github/workflows/publish-function.yml hack/fetch-verified.sh
  git commit --no-gpg-sign -m "ci: publish with main's workflow"
  ```

  The workflow reads the branch's tree only through the paths in its `env`
  (`crates/function-*/Dockerfile`, `package/wasm`, `package/oci`). If the
  branch doesn't have them, stop and report instead of adapting the file.
  `Supply Chain and Xpkg Extensions` needs nothing: it runs from `main`.
- **CI and the release matrix** (`ci.yml`, `e2e*.yml`,
  `release-matrix-e2e.yml`) stay as the branch has them. They call the
  branch's Makefile targets and build its `crates/e2e`, so `main`'s copies
  don't run on an older tree, and the branch's own matrix is the right gate
  for the branch's code. Starting the matrix by hand from `main`
  (`workflow_dispatch`) is no substitute: it tests the ref it runs on.

Know the shape of the matrix the push in step 5 starts. A line cut after
#346 has the matrix of its own function alone (`wasm / e2e (<profile>,
<version>)` or `oci / ...`, and the VM pairs for function-oci only). A line
cut before it, which on 2026-11-13 is `release-function-wasm-1.0`,
`release-function-oci-0.2` and everything older, has the earlier one: a
single run builds both runtimes (`build-tools`, `build-oci`), runs both
suites on each Crossplane version (`e2e (crossplane current|previous|oldest)`)
and all nine `vm suite (...)` legs, whichever function the branch releases.
It takes about fifteen minutes. Its jobs have no timeout, so a hung one
holds the run for six hours: watch it, and cancel and rerun it
(`gh run cancel`, `gh run rerun --failed`) instead of waiting. It also
saves build caches in the branch's own scope, about 2.6 GiB of the
repository's 10 GiB, which GitHub evicts a week after the last push. The
`guard` job of step 8 finds either matrix by its name, and
`hack/tested-versions.sh` reads either record.

### 2. Pull the CVE list from code scanning

Pass `ref` as an actual **query parameter**, not a client-side filter. The
list endpoint only returns alerts for the default branch unless `ref` is
given in the request itself - fetching all alerts and filtering afterward
with `select(.most_recent_instance.ref==...)` silently returns an empty
result even when open alerts exist for the tag:

```bash
gh api "repos/{owner}/{repo}/code-scanning/alerts?ref=refs/tags/${TAG}&state=open" \
  --paginate \
  --jq ".[] | {number, category: .most_recent_instance.category, rule: .rule.id, severity: .rule.security_severity_level, desc: .rule.description}"
```

On a tag both legs filed results under, the category tells the functions
apart: `.github/workflows/grype-scan.yml:scan` is function-wasm's,
`.github/workflows/grype-scan.yml:scan/function-oci` function-oci's.

For full detail (affected package, fixed-in version) on one alert:
`gh api repos/{owner}/{repo}/code-scanning/alerts/{alert_number}`. Triage
critical/high first.

The scan covers the base image alone. For the crates inside the binaries,
run `cargo deny check advisories` on the release branch: an advisory it
reports against `Cargo.lock` is a finding of this release too, whether or
not an alert names it. Keep the full list of both handy: step 4 checks it
against what actually got fixed.

### 3. Remediate on the release branch

**wasmtime.** A wasmtime advisory is fixed by bumping the wasmtime crates:
that is the sandbox's own security patch and belongs in this flow. Wasmtime
patches every major it supports, so the fix is normally a patch release of
the major the branch is on. The crates pin one another to exact versions and
move as one family: set `wasmtime`, `wasmtime-wasi`, `wasmtime-wasi-http` and
`wasi-preview1-component-adapter-provider` to the fixed release in
`crates/engine-wasm/Cargo.toml`, the only crate that imports them, and run
`cargo update --workspace`. `cargo update -p wasmtime --precise` on one of
them fails on the siblings' pins. `wit-component`, `wasmparser` and
`wit-parser` in the same file stay at what `cargo tree -i wit-component` shows
wasmtime pulling, which a patch release doesn't move. The compiled cache
needs nothing: `engine::version()` reads wasmtime's version from `Cargo.lock`.
Then run the guest suite with every toolchain installed, since it's the only
test that runs real guests of every language through the new engine:

```bash
GUESTFN_REQUIRE_TOOLCHAINS=1 cargo test --locked --release -p guestfn-runtime --test guests
```

Grype can't see a crate inside the `function` binary. A wasmtime advisory
arrives through `cargo deny` (the `deny` job of `ci.yml`, weekly in
`grype-scan.yml`), Renovate's alert or wasmtime's own advisories, never
through the scan.

A fix that exists only in a newer wasmtime **major** isn't a patch: the
engine's code in `crates/engine-wasm` follows the new API, and the WASI
versions the guests import can move with it. That is the **code-change
exception**: it lands on `main` (Renovate's `wasmtime` pull request) and
ships as a minor release through `/cut-release`. Step 4 says how to report
it.

A vulnerable crate that wasmtime merely *depends on* isn't covered by the
exception: `cargo update -p <crate> --precise <fixed-version>` moves it
inside the requirement wasmtime states, while wasmtime stays where it is.

For everything else the fix is one of:

- **Crate advisory** (any crate other than the wasmtime family, reported by
  `cargo deny check advisories` or a Renovate or GitHub alert, never by
  Grype) → `cargo update -p <crate> --precise <fixed-version>`. When the
  fixed version is outside the requirement a `Cargo.toml` states, raise the
  requirement there. A fix that needs the code that uses the crate changed
  falls under the code-change exception.
- **Rust standard library or toolchain CVE** → there is no pin to move. Both
  Dockerfiles build on `rust:${RUST_VERSION}` with `RUST_VERSION=1`, which
  floats, so the image `Publish Function` builds in step 8 is compiled with
  the stable release current that day. Confirm that it's the fixed one
  before tagging (`docker run --rm rust:1 rustc --version`). When this is
  the only finding the patch release is a rebuild of the branch's head, with
  no commit of its own: say so at the gate in step 4, and the user decides
  whether the rebuild ships.
- **Base image CVE** (`cgr.dev/chainguard/glibc-dynamic:latest@sha256:…`, one
  digest in `crates/function-wasm/Dockerfile`, `crates/function-oci/Dockerfile` and their
  `.dev` variants) →
  bump the pinned digest in all four to the current `latest`
  (`docker buildx imagetools inspect cgr.dev/chainguard/glibc-dynamic:latest`;
  Renovate's `runtime image` group opens these bumps too). One bump fixes
  both functions. Its glibc must stay at least as new as the one the `rust`
  build stage links against.

**function-oci's image** adds the init and the microVM pieces to the same
base, each fixed its own way:

- **The runtime and the init** (`/function`, `/usr/lib/guestfn/init`): crates
  of the same `Cargo.lock`, so a crate advisory is the `cargo update` above
  and patches both functions. `cargo deny` checks the graph with every
  feature on, the OCI engine's crates included.
- **Firecracker** (`/usr/lib/guestfn/firecracker`): move `FIRECRACKER_VERSION`
  in `crates/function-oci/Dockerfile` to the fixed release with both `FIRECRACKER_SHA256_*`
  checksums (Renovate's `firecracker` pull request does this), and with it
  `KERNEL_CI`, `KERNEL_VERSION` and both `KERNEL_SHA256_*` to the kernel that
  release's CI built. `hack/oci-tools.sh` reads the same pins, so
  `make oci.test` on a Linux host with `/dev/kvm` runs the engine's VM suite
  against them, and the push in step 5 runs the release matrix, whose oci
  profile boots them.
- **The guest kernel** (`/usr/lib/guestfn/vmlinux`): Firecracker's CI builds
  it, so a fix is a newer kernel from that CI, moved by hand with its two
  checksums as above.

This repository can't fix a Firecracker advisory with no fixed release, a
kernel CVE that no kernel from Firecracker's CI fixes yet, or anything in the
host's kernel and KVM, which the image doesn't contain. Report those as
skipped in step 4.

Then run what CI runs, in this order:

```bash
cargo build --workspace --locked && cargo test --workspace --locked
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo deny check advisories bans licenses sources   # the advisory is gone, and the bump brought no other
git status --porcelain                  # only Cargo.toml/Cargo.lock/crates/function-*/Dockerfile* should have moved
```

`cargo test --workspace` compares the conformance goldens, the scaffold
goldens and the Input CRD, and re-records none of them. A dependency bump
must not move any: if one fails, the bump changed what users see, so stop
and review that diff. Never run `UPDATE_GOLDENS=1` here to make it pass. The
workspace run skips a guest whose toolchain is missing, so after a wasmtime
bump also run the guest suite as above, and for function-oci, on a Linux
host with `/dev/kvm`, `make oci.test`.
Commit locally as `fix(security): ...`, naming the CVE/GHSA IDs
remediated and, if any were skipped under the exception above, listing them
in the commit body. Do not push yet - that's gated by step 4.

### 4. Completeness gate - decide what ships

Compare what got fixed against the full CVE list from step 2.

- **Every open CVE was remediated** → show the user the diff and the CVE
  list addressed, confirm before pushing, then continue to step 5. From here
  on, everything is externally visible (pushed branch, public tag, GitHub
  release, published package) and isn't something you can quietly undo by
  editing further.
- **Some CVEs were fixed, one or more were skipped under the code-change exception**
  → ship what was fixed: continue to step 5 with the same confirmation, and
  make the report in step 10 name the skipped ones. Unlike a fix that could
  land on the same patch line later, a code change can only ever ride a
  minor release from `main`, so holding the patch back would leave the line
  exposed for nothing. Report each skipped finding as:

  ```
  WARNING - not fixable by a dependency bump!
  <CVE/GHSA ID> requires a change that is not a dependency bump (<why>).
  Not applied by this flow. Fix it on main, then cut a minor release with
  /cut-release; the weekly scan of that release will confirm.
  ```

  A function-oci finding this repository can't fix (step 3) is skipped the
  same way and reported as:

  ```
  WARNING - no upstream fix yet!
  <CVE/GHSA ID> in function-oci's <Firecracker|guest kernel> has no fixed
  release. Not applied by this flow. Move the pins in
  crates/function-oci/Dockerfile when one exists (Renovate's firecracker pull request is the signal).
  ```

- **Nothing was fixed - no open CVE was a dependency bump** →
  **stop here.** Do not push, and do not create the tag or trigger `Publish
  Function`, or `Supply Chain and Xpkg Extensions` - an empty patch release
  remediates nothing. Leave `$BRANCH` as you found it and report the
  warning(s) above. The one exception is a Rust toolchain CVE, whose fix is
  the rebuild itself (step 3): nothing is committed, the patch tag goes on
  the branch's head as it is, and the user decides whether it ships.

### 4b. Draft the release notes for approval

Load the `writing-style` skill and draft the notes from
`../cut-release/release-notes-template.md`, filling its Security section: each
CVE or GHSA, what it affected and whether a reader was exposed. Say so if any
finding was skipped under the code-change exception, because the next weekly scan
reports it against this release again and that must not read as a regression.
Under the descriptions goes the full list of pull requests merged into the
release branch since `$TAG`, as `/cut-release` step 4 describes, generated
with `-f target_commitish="$BRANCH"` and copied whole.
Write the draft to a file outside the repository (`$NOTES`), show the user all
of it, and **stop until the user approves its text**. `$NOTES` doesn't change
after approval without asking again. Nothing is pushed before that.

### 5. Push

```bash
git push origin "$BRANCH"
```

### 6. Cut the patch tag: through the GitHub API, on the release branch

```bash
NEW_VERSION=$(echo "$VERSION" | awk -F. -v OFS=. '{$NF+=1} 1')   # v1.0.0 -> v1.0.1
NEW_TAG="$FUNCTION-$NEW_VERSION"
SHA=$(git rev-parse "origin/$BRANCH")
OBJ=$(gh api 'repos/{owner}/{repo}/git/tags' -f tag="$NEW_TAG" \
  -f message="Security patch: <CVE/GHSA summary>" -f object="$SHA" -f type=commit --jq .sha)
gh api 'repos/{owner}/{repo}/git/refs' -f ref="refs/tags/$NEW_TAG" -f sha="$OBJ" --jq .ref
```

### 7. Create the GitHub release

```bash
LATEST=false; [ "$FUNCTION" = function-wasm ] && LATEST=true
gh release create "$NEW_TAG" --verify-tag --target "$BRANCH" \
  --title "$NEW_TAG" --latest="$LATEST" \
  --notes-file "$NOTES"
```

(Step 6 created the tag and `--verify-tag` refuses to continue without it, so
`--target` only documents the branch.) `$NOTES` is the text approved in step
4b, unchanged.

### 8. Publish the package - `Publish Function`, from the new tag

```bash
gh workflow run "Publish Function" --ref "$NEW_TAG" -f function="$FUNCTION" -f version="$NEW_VERSION"
```

Must run from the **tag**, not the branch - it builds the runtime image from
that exact tagged source. `version` is the bare `vX.Y.Z`, the image's tag.

The workflow's `guard` job refuses a ref other than the tag, a version the
registry already has and a commit no `Release Matrix E2E` run passed on
(`-f without-matrix=true` and `-f overwrite=true` lift the last two, on the
owner's word alone).

### 9. Sign & attest - `Supply Chain and Xpkg Extensions`, from `main`

```bash
gh workflow run "Supply Chain and Xpkg Extensions" --ref main -f function="$FUNCTION" -f version="$NEW_VERSION"
```

Runs from `main`, not the tag - unlike step 8, this workflow doesn't build
anything from the ref it runs on. It signs/attests the image already
published to ghcr/xpkg.upbound.io by tag and appends Marketplace extensions
(SBOM, README, release notes), so it should use the newest signing logic on
`main` rather than whatever was frozen on the release branch at cut time.
A function-oci package gets the same run with `-f function=function-oci`.

### 9b. Record what was tested

For a function only. The release matrix recorded what it passed: update the
`README.md` table from that record and open the pull request.

```bash
RUN=$(gh run list --workflow "Release Matrix E2E" --branch "$BRANCH" --status success --limit 1 --json databaseId -q '.[0].databaseId')
RECORD=$(mktemp -d)
gh run download "$RUN" -n tested-versions -D "$RECORD"
git fetch origin main && git checkout -b "claude/tested-versions-$NEW_TAG" origin/main
hack/tested-versions.sh "$FUNCTION" "$NEW_VERSION" "$RECORD/tested-versions.json"
git add README.md && git commit --no-gpg-sign -m "docs: tested versions of $NEW_TAG"
git push -u origin HEAD
gh pr create --title "docs: tested versions of $NEW_TAG" \
  --body "Updates the tested-versions table for \`$NEW_TAG\` from [its release matrix run]($(gh run view "$RUN" --json url -q .url))."
```

### 10. Sequencing, verification, and reporting

Each of steps 6-9 must finish successfully before the next starts - step 8
needs the tag from step 6 to exist, step 9 needs the image step 8 published.
Find each run with `gh run list --workflow="<name>" --limit 1` and wait on it
(`gh run watch <run-id> --exit-status`); if using the Monitor tool, its
until-loop pattern is the sanctioned way to poll rather than a manual sleep
loop. Once all three conclude, optionally trigger `Grype Vulnerability Scan`
manually to confirm the new tag's image is clean (apart from any
code-change-exception findings; for function-oci, Grype's view covers the base and
nothing in Firecracker or the kernel), then report the CVE/GHSA IDs remediated, the new
version released, and every finding skipped under the code-change exception with
its warning block.

## Quick reference

| Workflow | Ref to run from | Key inputs |
|---|---|---|
| Tag through the GitHub API | head of the release branch (`release-<function>-X.Y`) | `$NEW_TAG`, message |
| (n/a) `gh release create` | n/a | `<function>-vX.Y.Z`, `--target` branch, `--latest` only for function-wasm |
| `Publish Function` | the new tag (`<function>-vX.Y.Z+1`) | `function` (`function-wasm` or `function-oci`), `version` (`vX.Y.Z+1`) |
| `Supply Chain and Xpkg Extensions` | `main` | `function` (`function-wasm` or `function-oci`), `version` |
| `Grype Vulnerability Scan` | `main` (scans each function's latest release image) | - |

## Common mistakes

- Bumping one wasmtime crate with `cargo update -p wasmtime --precise`: the
  family pins one another to exact versions, so the four versions in
  `crates/engine-wasm/Cargo.toml` move together.
- Taking a new wasmtime major as a patch: the engine's code and the guests'
  WASI versions move with it, which is a minor release from `main`.
- Skipping the guest suite after a wasmtime bump, or running it with a
  toolchain missing: it's the only test that runs real guests of every
  language through the new engine, and it skips a guest it can't build
  unless `GUESTFN_REQUIRE_TOOLCHAINS=1`.
- Reading a clean Grype scan as covering the crates: it sees none inside
  the binaries. `cargo deny check advisories` does.
- Cutting a patch release when nothing was fixable - an empty patch
  remediates nothing; halt at step 4 and report.
- Re-recording a golden (`UPDATE_GOLDENS=1`) to make a dependency bump pass:
  a conformance golden is the wording contract and a scaffold golden is
  what `guestfn init` writes, so a bump that moves one changed what users
  see.
- Using `gh release view` with no tag, which returns the repository's latest
  release of any component.
- Cutting the release branch from `main` HEAD instead of the release tag - leaks
  unreleased changes into a security patch.
- Running `Publish Function` against the branch instead of the new
  tag - builds a moving target if the branch gets more commits later.
- Running `Supply Chain and Xpkg Extensions` from the tag instead of `main`:
  it still works, but forfeits any signing/attestation fixes landed on
  `main` since the release branch was cut.
- Triggering step 8 or 9 before the prior workflow run has actually finished:
  the tag or image it depends on won't exist yet.
- Fetching `code-scanning/alerts` with no `ref` query parameter and filtering
  by `most_recent_instance.ref` afterward - this returns an empty list even
  when the tag has real open alerts, reading as "no CVEs found" instead of
  a query bug. Always pass `ref=refs/tags/<tag>` in the request itself.
- Replacing a release branch's CI or matrix workflows with `main`'s: they
  call Makefile targets and tests the older tree doesn't have. Only the
  publishing path is brought over (step 1b).
- Reading a run of `Release Matrix E2E` started by hand on `main` as the
  branch's matrix: it tested `main`.
- Reading a clean function-oci scan as covering Firecracker or the guest
  kernel: Grype sees neither. Check Firecracker's security advisories and
  the pins in `crates/function-oci/Dockerfile`.
- Bumping Firecracker without moving the kernel pins with it: the kernel is
  the one that Firecracker release's CI built.
