# Validate a Composition

Crossplane never installs a function's Input CRD, so the runtime is the only
gate a Composition's Input passes — and until now it was reached only by
reconciling. `function validate` runs that gate offline: the runtime binary
takes the same ceiling flags as when it serves and applies the same checks
(the Input's `compositionPolicy` compiled, `limits` against
`--module-timeout`/`--module-memory-limit`, the `module` source's shape)
to every function-wasm step of the Compositions (or bare `Input`
documents) you give it, printing the runtime's own words:

```shell
cargo run --release -p function-wasm -- validate \
  example/composition.yaml --module-dir=. --resolve
# or, with the released image (its entrypoint is the runtime):
docker run --rm -v "$PWD:/w" ghcr.io/jonasz-lasut/function-wasm:<version> validate /w/composition.yaml \
  --sandbox-policy-file /w/policy.cedar
```

```
composition.yaml: Composition/hello pipeline[0] hello: OK (oci ghcr.io/example/greeter:v1@sha256:3f2a…, limits timeout 5s memory 128Mi, compositionPolicy)
composition.yaml: Composition/hello pipeline[1] labeler: refused: module oci ghcr.io/example/labeler@sha256:9d1c… requires egress GET to host "evil.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit
```

`--xr xr.yaml` materialises `module.from` sources against that composite
resource, as the observed XR would (without it a `from` source is checked
for the `compositionPolicy` it requires and reported as the XR's
choice; a field the XR leaves unset under `module.allowEmpty` is reported
as no module, the step's no-op); `--resolve` goes on to resolve, verify (`--cosign-key`) and fetch
each module — OCI pulls use the local Docker config, never a step
credential — and compiles it with wasmtime for the runtime's own verdict
(size, ABI, host imports; a compile is seconds and about a gigabyte for a
large Go module), then decides its manifest's `requires` as the runtime
does and lists, on a `credentials:` line, the step credentials its request
would carry (with none listed it receives none); `--function-name` keeps only the steps of one function; `--output json` prints one JSON object per step for
CI annotations; `-` reads stdin. Warnings (a `Path` source in a
Composition, egress granted without `--cosign-key`, a limit equal to its
ceiling, a field the runtime would silently ignore, the deprecated
`v1beta1` apiVersion) are printed under the step and never change the
exit code: 0 when every step is admitted, 1 when
at least one is refused, 2 when the tool itself failed (unreadable file,
unparsable YAML, a bad flag). `make -C examples/pdb-addon render` runs it
over the example first.
