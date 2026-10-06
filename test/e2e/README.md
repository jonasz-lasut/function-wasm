# End-to-end tests

Scenarios that exercise the runtime against real infrastructure: a real
registry, the real cosign CLI, `crossplane composition render`. The unit and
integration tests under `crates/` use in-memory stand-ins for these (the
`oci::testregistry`, hand-built cosign bundles); a scenario here is where the
real thing has to agree with them.

| Tier | Where | What runs |
|---|---|---|
| Unit and integration | `crates/*` (`cargo test`) | the runtime, the engine and guestfn in-process, against stand-ins |
| Render | `examples/*/example/xprin.yaml`, `test/e2e/<scenario>/` | the runtime binary served to `crossplane composition render`, with real registries and signatures where a scenario needs them |
| Cluster | not yet | Crossplane in a kind cluster installing the package: mTLS, the operator policy from a mounted ConfigMap, readiness and warm-up, in-cluster egress. The tool is open: kyverno/chainsaw, or the Crossplane CLI's own test command once it ships upstream |

Every render-tier scenario runs in the `/e2e` workflow (`.github/workflows/e2e.yml`).

## Conventions

- **A shell script sets the stage; data asserts.** `run.sh` starts what the
  scenario needs (a registry, the runtime), pushes and signs, and fills
  generated values (digests) into the manifests. What is checked lives in an
  xprin suite (rendered resources and XR status) and in expected verdicts of
  `function validate --output json`, asserted on its fields with `jq`. A
  refusal is a fatal result, which fails a render, so refusals are checked
  through validate, the runtime's own admission.
- **Nothing outside the run is touched.** A scenario uses its own Docker
  config (`DOCKER_CONFIG`), throwaway containers and a temp directory, all
  removed on exit.
- **Prebuilt tools when given:** `FUNCTION_BIN`, `GUESTFN` and `XPRIN`, as the
  render jobs use them; the runtime is built from the tree otherwise.

## Scenarios

- [`oci/`](oci): the OCI path (#112). `examples/pdb-addon` is pushed to a
  registry behind basic auth and signed with cosign; a `WebApp` names it by
  digest through `module.from`, and the runtime pulls it with the step
  credential the WebApp names, behind the `compositionPolicy`'s `pullModule`
  fence and the operator policy's `requireSignature`. Run it with
  `test/e2e/oci/run.sh` (docker, the crossplane CLI, cosign 3.1+, jq, xprin,
  guestfn and Go).
