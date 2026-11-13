---
name: cut-release
description: Use when asked to cut a new minor or major release of function-wasm, function-oci, guestfn or the WIT world, ship a batch of merged main changes as a new version, or bump to a specific vX.Y.0 / vX.0.0: for example "cut a minor release of function-oci", "ship guestfn v0.2.0", "release the next major version", "publish the WIT world". Not for patch releases remediating CVEs (see remediate-cves) or ad hoc patch bumps.
---

# Cutting a Minor or Major Release

## Overview

Cutting a minor or major release ships what is merged to `main` as a
brand-new version line of one **component**. Each component has its own
versions, git tags, GitHub releases and release branches:

| Component | Git tag and GitHub release | Release branch | Published as |
|---|---|---|---|
| `function-wasm` | `function-wasm-vX.Y.Z` | `release-function-wasm-X.Y` | `ghcr.io/jonasz-lasut/function-wasm:vX.Y.Z` |
| `function-oci` | `function-oci-vX.Y.Z` | `release-function-oci-X.Y` | `ghcr.io/jonasz-lasut/function-oci:vX.Y.Z` |
| `guestfn` | `guestfn-vX.Y.Z` | `release-guestfn-X.Y` | binaries attached to the GitHub release |
| `wit` | `wit-vX.Y.Z` | `release-wit-X.Y` | `ghcr.io/jonasz-lasut/guestfn/function:X.Y.Z` |

An OCI artifact's tag drops the component prefix and keeps the version. The
WIT world's is the bare package version, the form `wkg` writes. The
published examples have no git tags or releases: `Publish Examples` runs on
its own lifecycle.

Unlike a CVE security patch (see `remediate-cves`), there is no fix to
author: the branch is cut straight from `main` HEAD, and the component and
the bump (minor or major) are chosen by the user, never inferred.

Every release branch **stays**: cutting a new line never deletes a
superseded one, at any version, because the repository keeps its full
release history (Jonasz, 2026-11-10). Through a component's 0.x line CVE
remediations target its latest branch alone. From its v1.0.0 on, an older
line can still take a patch.

## When to use

- Asked to cut/ship a new minor or major release, "release vX.Y.0", "bump to
  vX.0.0", or ship a batch of merged features as a new version.
- **Not** for patch releases remediating CVEs/security findings on an
  already-released version - that reuses an *existing* release branch cut
  from a tag (see `remediate-cves`).
- **Not** for ad hoc patch-level bumps (`vX.Y.Z+1`) with no security finding
  behind them - this skill only computes minor or major bumps.
- The component and the bump type (minor or major) must come from explicit
  user input. Never infer them from the diff: ask if they aren't stated.

## Procedure

### 1. Determine current version and confirm bump type

```bash
COMPONENT=function-wasm   # or function-oci, guestfn, wit: from the user
TAG=$(gh release list --limit 200 --json tagName -q '.[].tagName' \
  | grep -E "^${COMPONENT}-v[0-9]+\.[0-9]+\.[0-9]+$" | sort -V | tail -n 1)
VERSION=${TAG#"$COMPONENT-"}   # e.g. v1.0.0; empty on the component's first release
MAJOR=$(echo "$VERSION" | sed -E 's/^v([0-9]+)\.([0-9]+)\..*/\1/')
MINOR=$(echo "$VERSION" | sed -E 's/^v([0-9]+)\.([0-9]+)\..*/\2/')
```

Never use `gh release view` with no tag: the repository's latest release can
be another component's.

If the user has already stated minor or major (e.g. "cut a minor release"),
use that directly. Otherwise ask before computing anything. Then:

- **major**: `NEW_VERSION="v$((MAJOR+1)).0.0"`
- **minor**: `NEW_VERSION="v${MAJOR}.$((MINOR+1)).0"`
- **a component's first release** (`TAG` is empty): ask the user for the version.
- **`wit`**: the version isn't a bump, it's the world's own:
  `NEW_VERSION="v$(sed -nE 's/^package guestfn:function@([^;]+);$/\1/p' wit/guestfn-function.wit)"`.
  Stop if that version has a release already or is a pre-release.

```bash
NEW_TAG="$COMPONENT-$NEW_VERSION"
NEW_BRANCH="release-$COMPONENT-$(echo "$NEW_VERSION" | sed -E 's/^v([0-9]+)\.([0-9]+)\..*/\1.\2/')"
```

### 2. Verify `main` is release-ready

```bash
git fetch origin
git checkout main && git pull
gh run list --workflow CI --branch main --limit 1     # confirm the latest CI run on main is green
```

There is no remediation step here - whatever is merged to `main` is what
ships, and unlike CVE remediation there is no fix step or completeness gate
downstream to catch a bad `main` later. If the latest run's conclusion isn't
`success`, **stop and report** - do not cut the branch until `main` is green.
CI on `main` covers lint (cargo fmt + clippy), the workspace tests
(conformance and scaffold goldens included), `cargo deny`, the guest suite
and the OCI engine's VM suite, so a green run is a legitimate signal. Name
the workflow: the latest run on `main` of any workflow can be `E2E Caches`
or the docs deploy. The cluster profiles and the example build jobs don't
run on `main`: they ran on the pull requests labelled `e2e:`, and the
profiles run again in step 5b.

**For a function, bump the runtime-version references in docs to
`$NEW_VERSION` before cutting** (there is no automated step for this): the
package tags in `README.md` and `docs/wasm/install.md` or `docs/oci/install.md`
(the Install snippet) and
`examples/deployment-runtime-config-cedar.yaml`,
and any example whose Composition installs the function, should name the version
being released, not the previous one - a copy-pasted install of a tag that does
not exist yet fails. Find them with `git grep -n "$COMPONENT:v[0-9]"` and
commit the bump to `main` before step 3, so CI stays green and the new branch
carries it. Not this: illustrative `ghcr.io/example/...` module tags
(arbitrary). Guests vendor their ABI glue under `internal/wasmfn`, so there is
no separate SDK version to bump.

**For `guestfn`, move the version in the install snippet of `README.md` and
`docs/wasm/write-a-module.md` to `$NEW_VERSION`, and refresh the scaffold
templates' dependency pins, before cutting** (`SECURITY.md`: users ship what
`guestfn init` writes; Renovate cannot read `.tmpl`). Move each to its latest
release: function-sdk-go
(`FALLBACK_SDK_VERSION` in `crates/guestfn/src/main.rs`, plus the
`sdk_version` in `scaffold.rs`'s tests), the go `go.mod.tmpl`
(its grpc floor too, dropped once a function-sdk-go release
requires grpc v1.83 or newer),
the rust `Cargo.toml.tmpl`, the ts
`package.json.tmpl`'s devDependency ranges (then `npm install` in
`examples/function-wasm-ts` so its lockfile, which the template does not carry,
satisfies them), and the python `requirements.txt` (componentize-py and
protobuf; the protobuf runtime refuses a codec from a newer protoc, so its
`7.X.Y` must not fall behind the root `Makefile`'s `PROTOC_VERSION` `X.Y`).
The zig, c, cpp and odin examples' `build.zig.zon` (`zig fetch --save <url>`
writes url and hash), which no Renovate manager reads either, move in the
same pass (the zig and c templates retired in #264), and so does the
MoonBit toolchain `e2e-wasm.yml` pins (`MOON_VERSION` with the checksums of
its two tarballs, and `moonbitlang/protobuf` in the example's `moon.mod`),
which has no datasource for Renovate to read.
Mirror each change into its example (`render_matches_the_examples` fails on
drift), run `UPDATE_GOLDENS=1 cargo test -p guestfn`, and merge the PR to
`main` once CI and the end-to-end run are green: label the PR `e2e:all`,
and `e2e-gate` is its verdict.

### 3. Cut the new release branch from `main` HEAD

```bash
if git ls-remote --exit-code --heads origin "$NEW_BRANCH"; then
  echo "ALREADY EXISTS - abort: this version line was already released"
else
  git checkout -b "$NEW_BRANCH" main
fi
```

If the branch already exists, **stop and report** rather than continuing -
do not fall through to checkout/push.

This is the mirror image of the CVE flow: a security patch reuses an
*existing* release branch cut from a tag; a new minor/major line is always a
**brand-new** branch cut from `main` HEAD. If `origin/$NEW_BRANCH` already
exists, this version line was already released - stop and report instead of
pushing over it.

The new branch carries the workflows of `main` as they are at the cut, and
runs them from then on: a later change to the workflows on `main` doesn't
reach the line. `remediate-cves` step 1b says what a patch release brings
over and what it leaves.

### 4. Draft the release notes and confirm before anything externally visible

Load the `writing-style` skill and draft the notes from
`release-notes-template.md` beside this file, for a reader deciding whether to
upgrade and what it will take. The notes have two parts.

The descriptions come first. Work from what the pull requests merged since
`$TAG` changed (`git log --oneline "$TAG"..main`), not from their titles.
Every change a reader would notice gets a `###` heading of its own and a
paragraph under it: what the component does now and what it did before, why
it changed, and what the reader does about it, with the exact flag, field or
refusal quoted and a link to the docs page and the pull request. A one-line
bullet per change isn't enough. Delete any section with nothing to say, and
describe nothing that changes nothing for that reader: dependency bumps with
no change in behaviour, refactors, CI and test-only work.

The full list of merged pull requests comes under the descriptions, and
leaves nothing out, whatever the descriptions skipped. GitHub writes it:

```bash
gh api "repos/{owner}/{repo}/releases/generate-notes" \
  -f tag_name="$NEW_TAG" -f target_commitish=main \
  ${TAG:+-f previous_tag_name="$TAG"} --jq .body
```

Copy its `* <title> by @<author> in <URL>` lines under `## Merged pull
requests` as they are, every one, and its compare link as the last line. The
range is the repository's, so the list includes pull requests that touched
another component alone: keep them, the list is what merged between the two
tags. On a component's first release `$TAG` is empty and the list is
everything GitHub finds before the tag.

Write the draft to a file outside the repository (`$NOTES`).

Show the user `NEW_TAG`, `NEW_BRANCH` and the whole draft, then **stop until
the user approves the text of the notes**. Agreeing to the release isn't
approving its notes, and `$NOTES` doesn't change after approval without asking
again. Release notes are what a user reads to decide whether to upgrade, so
they get the same review as the code. From here on every step is
externally visible (pushed branch, public tag, GitHub release, published
package) and isn't something you can quietly undo - get explicit
confirmation before continuing.

### 5. Push

```bash
git push -u origin "$NEW_BRANCH"
```

### 5b. Wait for the release matrix - `Release Matrix E2E`, on the new branch

For a function only: `guestfn` and `wit` have no matrix, so go to step 6. The
push starts it (`.github/workflows/release-matrix-e2e.yml` runs on every
push to a `release-function-wasm-*` or `release-function-oci-*` branch, #189,
and tests the function the branch names alone; its `workflow_dispatch`, which
takes `-f runtime=wasm|oci`, is for trying the matrix on another ref, not for
releasing). Find the run the push started and
wait for it, as for green CI:

```bash
sleep 30   # the run appears a few seconds after the push
RUN=$(gh run list --workflow "Release Matrix E2E" --branch "$NEW_BRANCH" --limit 1 --json databaseId -q '.[0].databaseId')
gh run watch "$RUN" --exit-status
```

The workflow builds the branch's runtime once and runs every profile of
that runtime's end-to-end cluster suite on a cluster of its own per
Crossplane version, nine jobs in parallel (`wasm / e2e (<profile>,
<version>)` or `oci / ...`): on the Makefile's `CROSSPLANE_VERSION` default,
what a pull request's end-to-end run tests, and on the two older minors
pinned in the workflow's `env` - the three minors Crossplane maintains
(docs.crossplane.io/latest/learn/release-cycle). A `function-oci` branch also
runs the OCI engine's VM suite on nine Firecracker and kernel pairs, of which
only the pair the image pins blocks; a `function-wasm` branch runs no VM. It
takes about twenty minutes. Nothing externally visible beyond the branch has happened
yet, so a failure here is cheap: **a red version is a release blocker**.
Stop and report which version failed and why (its
`e2e-<runtime>-dump-<profile>-<version>` artifact holds the cluster dump) - either the
branch gets a fix (merged to `main`, the branch re-cut, the push starting the
matrix again) or the user decides the version is no longer supported, in
which case `docs/project/testing.md` states the new floor and the pins move
before the release continues. Never cut the tag over a red matrix. On a
release line cut before #189 merged the workflow does not exist on the
branch and no run appears: say so and continue.

### 6. Cut the tag: through the GitHub API, on the new release branch

```bash
SHA=$(git rev-parse "origin/$NEW_BRANCH")
OBJ=$(gh api 'repos/{owner}/{repo}/git/tags' -f tag="$NEW_TAG" \
  -f message="Release $NEW_TAG" -f object="$SHA" -f type=commit --jq .sha)
gh api 'repos/{owner}/{repo}/git/refs' -f ref="refs/tags/$NEW_TAG" -f sha="$OBJ" --jq .ref
```

That creates the annotated `$NEW_TAG` on the branch's head, with no local tag
and no signature. Verify it exists before publishing:
`git ls-remote --tags origin "$NEW_TAG"`. There is no separate guest-SDK
tag: each guest vendors its ABI glue under `internal/wasmfn` (the scaffold
writes it), so nothing needs `go get`-ing at a pinned version.

### 7. Create the GitHub release

```bash
LATEST=false; [ "$COMPONENT" = function-wasm ] && LATEST=true
gh release create "$NEW_TAG" --verify-tag --target "$NEW_BRANCH" \
  --title "$NEW_TAG" --latest="$LATEST" \
  --notes-file "$NOTES"
```

`$NOTES` is the text approved in step 4, unchanged.

GitHub marks one release Latest, and that's function-wasm's, so another
component's release never replaces it.

(Step 6 created the tag and `--verify-tag` refuses to continue without it, so
`--target` only documents the branch.)

A `function-wasm` release's notes state what it serves, as `docs/abi-v2.md`'s versioning
policy promises: the world (`wit/guestfn-function.wit`'s package line),
wasmtime (its entry in `Cargo.lock`) and the WASI interface versions the
scaffolds pin in `guestfn.lock`, which are what a guest imports beside
the world. Read them from the tree being released:

```bash
sed -nE 's/^package (guestfn:function@[^;]+);/\1/p' wit/guestfn-function.wit
rg -n '^name = "wasmtime"$' -A1 Cargo.lock | rg -o 'version = ".*"'
rg -I --no-heading -A1 '^  - name: wasi:(cli|clocks|http|io)$' crates/guestfn/templates/*/guestfn.lock \
  | rg -o '(wasi:[a-z]+|[0-9]+\.[0-9]+\.[0-9]+)' | paste - - | sort -u
```

One sentence in the notes carries them, for example: `Serves
guestfn:function@1.0.0 on wasmtime 49.0.2: WASI 0.2.12 (cli, clocks, http,
io) for the sync scaffolds and WASI 0.3.0 (clocks, http) for the rust
scaffold.` A guest keeps running as long as the runtime's wasmtime serves
the versions it was built against, so a release that moves any of them
says which, and what a guest does about it (move the imports in
`wit/world.wit`, run `guestfn wit update`, rebuild).

### 8. Publish: from the new tag

```bash
case "$COMPONENT" in
  function-*) gh workflow run "Publish Function" --ref "$NEW_TAG" -f function="$COMPONENT" -f version="$NEW_VERSION" ;;
  guestfn)    gh workflow run "Publish guestfn" --ref "$NEW_TAG" -f version="$NEW_VERSION" ;;
  wit)        gh workflow run "Publish WIT" --ref "$NEW_TAG" ;;
esac
```

Must run from the **tag**, not the branch, so it builds from that exact
tagged source. `Publish Function` checks this itself in its `guard` job: it
refuses a ref other than `<function>-<version>`'s tag, a version the
registry already has, and a commit no `Release Matrix E2E` run passed on.
Two inputs lift the last two, each only on the owner's word:
`-f without-matrix=true` for a release whose matrix the owner chose not to
run, and `-f overwrite=true` to publish a version again after a publish
that failed part-way. `Publish guestfn` refuses any other ref and attaches the
binaries to the GitHub release of step 7. `version` is the bare `vX.Y.Z`,
which is the OCI tag and the version stamped into the binary.

`Publish Function` also signs what it published, in the same run: its last
job, `sign`, calls `supplychain.yml` as it is on `main` with the digest the
`push` job pushed. It signs and attests that digest in both registries,
never the tag, after checking that the tag still resolves to it, and appends
the Marketplace extensions (SBOM, README, release notes) to the mirror. The
signing logic is `main`'s whatever the tag carries, and the signer identity
is `supplychain.yml@refs/heads/main` for every release. There is no separate
signing step in a normal release: wait for the whole run, `sign / sign-artifacts`
included.

The use-case examples have a lifecycle of their own: `Publish Examples`
(`publish-examples.yml`), run by hand from `main` when an example's
`guestfn.yaml` `version` moved, and never part of a release.

### 9. Sign again, only when needed: `Supply Chain and Xpkg Extensions`, from `main`

Not part of a normal release. `Publish guestfn` and `Publish WIT` sign and
attest what they publish, and since `Publish Function` signs in its own run
this step is for a function release whose `sign` job failed after the push,
or one published before that job existed:

```bash
gh workflow run "Supply Chain and Xpkg Extensions" --ref main -f function="$COMPONENT" -f version="$NEW_VERSION"
```

Run by hand it takes no digest and signs whatever the version's tag resolves
to in each registry, so check first that the tag is the package the release
pushed (the `push` job's summary has the digest).

### 10. Sequencing and verification

Each of steps 6-8 must finish successfully before the next starts: step 8
needs the tag from step 6 to exist, and its `sign` job needs the GitHub
release of step 7 for the release notes it appends.
Find each workflow's run with `gh run list --workflow="<name>" --limit 1` and wait on it
(`gh run watch <run-id> --exit-status`); if using the Monitor tool, its
until-loop pattern is the sanctioned way to poll rather than a manual sleep
loop. Only once all of them have concluded successfully, move on to the report.

### 10b. Record what was tested

For a function only. The release matrix recorded what it passed: update the
`README.md` table from that record and open the pull request.

```bash
RUN=$(gh run list --workflow "Release Matrix E2E" --branch "$NEW_BRANCH" --status success --limit 1 --json databaseId -q '.[0].databaseId')
RECORD=$(mktemp -d)
gh run download "$RUN" -n tested-versions -D "$RECORD"
git fetch origin main && git checkout -b "claude/tested-versions-$NEW_TAG" origin/main
hack/tested-versions.sh "$COMPONENT" "$NEW_VERSION" "$RECORD/tested-versions.json"
git add README.md && git commit --no-gpg-sign -m "docs: tested versions of $NEW_TAG"
git push -u origin HEAD
gh pr create --title "docs: tested versions of $NEW_TAG" \
  --body "Updates the tested-versions table for \`$NEW_TAG\` from [its release matrix run]($(gh run view "$RUN" --json url -q .url))."
```

### 11. Report

Report the new tag released and the branch it lives on. Leave every earlier
release branch where it is: the repository keeps its full release history.

## Quick reference

| Step | Ref to run from | Key inputs |
|---|---|---|
| Cut branch | `main` HEAD | new `release-<component>-X.Y` |
| `Release Matrix E2E` (functions) | started by the push of `release-function-*` | none; `gh run watch --exit-status` on it |
| Tag through the GitHub API | head of the new release branch | `$NEW_TAG`, message |
| (n/a) `gh release create` | n/a | `<component>-vX.Y.Z`, `--target` branch, `--latest` only for function-wasm |
| `Publish Function` (functions) | the new tag | `function`, `version` (`vX.Y.Z`) |
| `Publish guestfn` | the new tag | `version` (`vX.Y.Z`) |
| `Publish WIT` | the new tag | none |
| `Supply Chain and Xpkg Extensions` (only to sign again) | `main` | `function`, `version` (`vX.Y.Z`) |

## Common mistakes

- Cutting the release branch from an existing tag instead of `main` HEAD -
  that's the CVE-patch pattern; a new minor/major line must include
  everything merged to `main`.
- Inferring the component, or minor vs major, from the diff instead of the
  user's explicit input.
- Using `gh release view` with no tag, which returns the repository's latest
  release of any component.
- Passing the prefixed tag as `version` to `Publish Function` or the supply
  chain workflow: the image is tagged with the version alone, `vX.Y.Z`.
- Not checking whether `NEW_BRANCH` already exists - each release line
  is cut exactly once; if it exists, this version was already released.
- Cutting from a red `main` - unlike CVE remediation there's no fix step or
  completeness gate to catch this, so a broken `main` ships broken as-is.
- Cutting the tag before the release matrix is green on the branch, or
  skipping it: a pull request's end-to-end run covered the current
  Crossplane only; the two older minors are tested nowhere else.
- Running `Publish Function` against the branch instead of the new
  tag - builds a moving target if the branch gets more commits later.
- Starting `Supply Chain and Xpkg Extensions` after a `Publish Function`
  run that signed already: the publish run's `sign` job did it, by digest.
- Running `Supply Chain and Xpkg Extensions` from a tag or a branch instead
  of `main`: the signer identity then names that ref, and users pin
  `supplychain.yml@refs/heads/main`.
- Triggering step 8 before the tag and the GitHub release exist.
- Release notes that do not state the served world, wasmtime and WASI
  versions: the versioning policy in `docs/abi-v2.md` promises them.
- Using this skill for a patch bump (`vX.Y.Z+1`) - that path belongs to
  `remediate-cves`, which reuses an existing branch instead of cutting one.
- Release notes that describe a change in one line, or whose `Merged pull
  requests` list is trimmed, reworded or missing.
- Publishing release notes the user hasn't approved, or editing them after
  approval without asking again.
- Deleting a release branch, of any version or any component, superseded or
  not: the repository keeps its full release history.
