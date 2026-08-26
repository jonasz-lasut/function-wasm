---
name: remediate-cves
description: Use when asked to remediate CVEs, security vulnerabilities, or GitHub code-scanning/Grype findings against a function-wasm release — e.g. "fix the CVEs from grype", "patch the vulnerabilities on the latest release", a security alert on a release tag, or a request to cut a security patch release.
---

# Remediating CVEs

## Overview

`.github/workflows/grype-scan.yml` runs weekly (Mondays 05:00 UTC), scans the
ghcr image of the **latest GitHub release** with Grype, and uploads the SARIF
results to GitHub code scanning against `refs/tags/<that release's tag>`.
Remediating a CVE found this way means patching the released version's branch
and shipping a new patch release end to end: branch → fix → tag → release →
publish → sign. What ships is decided by the completeness gate in step 4.

## When to use

- Asked to remediate/fix CVEs, vulnerabilities, GHSA advisories, or grype /
  code-scanning findings tied to a release of this function.
- **Not** for routine dependency bumps with no security alert behind them
  (that's ordinary `chore(deps)` / Renovate work).
- Bumping the `wasmtime` crate **is** in scope — see step 3;
  an wasm bump is a minor release from `main` (`/cut-release`), never a patch.

## Procedure

### 1. Identify the target release branch

```bash
TAG=$(gh release view --json tagName -q .tagName)               # e.g. v1.0.0
BRANCH=release-$(echo "$TAG" | sed -E 's/^v([0-9]+)\.([0-9]+)\..*/\1.\2/')  # release-1.0
git fetch origin
git checkout "$BRANCH" 2>/dev/null || git checkout -b "$BRANCH" "$TAG"
```

If `origin/$BRANCH` doesn't exist yet, it must be cut from the release **tag**,
never from `main` HEAD — a security patch must not carry unreleased changes.

### 2. Pull the CVE list from code scanning

Pass `ref` as an actual **query parameter**, not a client-side filter. The
list endpoint only returns alerts for the default branch unless `ref` is
given in the request itself — fetching all alerts and filtering afterward
with `select(.most_recent_instance.ref==...)` silently returns an empty
result even when open alerts exist for the tag:

```bash
gh api "repos/{owner}/{repo}/code-scanning/alerts?ref=refs/tags/${TAG}&state=open" \
  --paginate \
  --jq ".[] | {number, rule: .rule.id, severity: .rule.security_severity_level, desc: .rule.description}"
```

For full detail (affected package, fixed-in version) on one alert:
`gh api repos/{owner}/{repo}/code-scanning/alerts/{alert_number}`. Triage
critical/high first. Keep this full list handy — step 4 checks it against
what actually got fixed.

### 3. Remediate on the release branch

**wasmtime.** A wasmtime advisory is fixed by bumping the `wasmtime` and
`wasmtime-wasi` crates — that is the sandbox's own security
patch and belongs in this flow. Every wasmtime release is a new Go major with
a new crate major: `cargo update` (or bump the version in
`crates/engine/Cargo.toml`) and update the
single import in `internal/engine`. Then run the full root test suite without
`-short` — it builds `examples/hello-go` to wasm and runs it through the new
runtime — plus `go test ./...` in `examples/hello-go`. Note that
syft/grype cannot see the Rust code inside the prebuilt libwasmtime; watch
wasmtime's own advisories, not only the scan.

A vulnerable module that wasm merely *depends on* is not covered by the
exception: `cargo update <crate> --precise <fixed-version>` — the resolver takes the
higher requirement while wasm itself stays where it is.

For everything else, Grype scans the built ghcr image, so the fix is one of:

- **Go module CVE** (any module other than wasm) →
  `cargo update <crate> --precise <fixed-version>`.
- **Go stdlib/toolchain CVE** → bump the `go` directive in `go.mod`. It is
  the single source of truth: `ci.yml` sets up Go from it and
  `publish-pkg.yml` reads it to pick the `golang:<version>` image the runtime
  is built with, so nothing else needs editing.
- **Base image CVE** (`cgr.dev/chainguard/glibc-dynamic:latest@sha256:…` in
  the Dockerfile) → bump the pinned digest to the current `latest`
  (`docker buildx imagetools inspect cgr.dev/chainguard/glibc-dynamic:latest`;
  Renovate opens these bumps too). Its glibc must stay at least as new as
  the one `golang:<version>` builds against.

Then run what CI runs, in this order:

```bash
cargo build --workspace --locked && cargo test --workspace
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
git status --porcelain                  # only go.mod/go.sum/Dockerfile should have moved
```

`go generate ./...` regenerates the Input CRD and the `guestfn` scaffold golden
under `cmd/guestfn/internal/scaffold/testdata/golden/`. A dependency bump must
not change either; if the golden moves, the scaffold users get changed — stop
and review that diff before deciding to keep it. Also run the example module
(`cd examples/hello-go && go test ./...`, which covers its vendored
internal/wasmfn glue) and the root tests without `-short` (they build the
example guest to wasm).
Commit locally as `fix(security): ...`, naming the CVE/GHSA IDs
remediated and, if any were skipped under the exception above, listing them
in the commit body. Do not push yet — that's gated by step 4.

### 4. Completeness gate — decide what ships

Compare what got fixed against the full CVE list from step 2.

- **Every open CVE was remediated** → show the user the diff and the CVE
  list addressed, confirm before pushing, then continue to step 5. From here
  on, everything is externally visible (pushed branch, public tag, GitHub
  release, published package) and isn't something you can quietly undo by
  editing further.
- **Some CVEs were fixed, one or more were skipped under the wasm exception**
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

- **Nothing was fixed — no open CVE was a dependency bump** →
  **stop here.** Do not push, and do not trigger `Tag`, `Publish Function
  Package`, or `Supply Chain and Xpkg Extensions` — an empty patch release
  remediates nothing. Leave `$BRANCH` as you found it and report the
  warning(s) above.

### 5. Push

```bash
git push origin "$BRANCH"
```

### 6. Cut the patch tag — `Tag` workflow, from the release branch

```bash
NEW_VERSION=$(echo "$TAG" | awk -F. -v OFS=. '{$NF+=1} 1')   # v1.0.0 -> v1.0.1
gh workflow run Tag --ref "$BRANCH" -f version="$NEW_VERSION" \
  -f message="Security patch: <CVE/GHSA summary>"
```

### 7. Create the GitHub release

```bash
gh release create "$NEW_VERSION" --target "$BRANCH" \
  --title "$NEW_VERSION" \
  --notes "Security patch release. Remediates: <CVE/GHSA list>."
```

(`--target` only matters if the tag doesn't already exist; the `Tag` workflow
already created it, so this is just documentation-by-flag.) If findings were
skipped under the wasm exception, say so in the notes — the next weekly scan
will report them against this release again, and that must not read as a
regression.

### 8. Publish the package — `Publish Function Package`, from the new tag

```bash
gh workflow run "Publish Function Package" --ref "$NEW_VERSION" -f version="$NEW_VERSION"
```

Must run from the **tag**, not the branch — it builds the runtime image from
that exact tagged source, with the Go version that tag's `go.mod` declares.

### 9. Sign & attest — `Supply Chain and Xpkg Extensions`, from `main`

```bash
gh workflow run "Supply Chain and Xpkg Extensions" --ref main -f version="$NEW_VERSION"
```

Runs from `main`, not the tag — unlike step 8, this workflow doesn't build
anything from the ref it runs on. It signs/attests the image already
published to ghcr/xpkg.upbound.io by tag and appends Marketplace extensions
(SBOM, README, release notes), so it should use the newest signing logic on
`main` rather than whatever was frozen on the release branch at cut time.

### 10. Sequencing, verification, and reporting

Each of steps 6–9 must finish successfully before the next starts — step 8
needs the tag from step 6 to exist, step 9 needs the image step 8 published.
Find each run with `gh run list --workflow="<name>" --limit 1` and wait on it
(`gh run watch <run-id> --exit-status`); if using the Monitor tool, its
until-loop pattern is the sanctioned way to poll rather than a manual sleep
loop. Once all three conclude, optionally trigger `Grype Vulnerability Scan`
manually to confirm the new tag's image is clean (apart from any
wasm-exception findings), then report the CVE/GHSA IDs remediated, the new
version released, and every finding skipped under the wasm exception with
its warning block.

## Quick reference

| Workflow | Ref to run from | Key inputs |
|---|---|---|
| `Tag` | release branch (`release-X.Y`) | `version`, `message` |
| (n/a) `gh release create` | — | tag name, `--target` branch |
| `Publish Function Package` | the new tag (`vX.Y.Z+1`) | `version` |
| `Supply Chain and Xpkg Extensions` | `main` | `version` |
| `Grype Vulnerability Scan` | `main` (scans the latest release) | — |

## Common mistakes

- Bumping wasmtime without checking the engine's API usage in
  `internal/engine` — the old major stays in go.mod and nothing changed.
- Running the tests with `-short` after a wasmtime bump — that skips the only
  test that runs a real Go guest through the new runtime.
- Cutting a patch release when nothing was fixable — an empty patch
  remediates nothing; halt at step 4 and report.
- Committing a scaffold golden change as part of a "pure" dependency bump
  without reading it — that diff is what `guestfn init` will generate.
- Cutting `release-X.Y` from `main` HEAD instead of the release tag — leaks
  unreleased changes into a security patch.
- Running `Publish Function Package` against the branch instead of the new
  tag — builds a moving target if the branch gets more commits later.
- Running `Supply Chain and Xpkg Extensions` from the tag instead of `main`
  — it still works, but forfeits any signing/attestation fixes landed on
  `main` since the release branch was cut.
- Triggering step 8 or 9 before the prior workflow run has actually finished
  — the tag or image it depends on won't exist yet.
- Fetching `code-scanning/alerts` with no `ref` query parameter and filtering
  by `most_recent_instance.ref` afterward — this returns an empty list even
  when the tag has real open alerts, reading as "no CVEs found" instead of
  a query bug. Always pass `ref=refs/tags/<tag>` in the request itself.
