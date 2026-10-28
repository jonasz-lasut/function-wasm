# A request, step by step

1. The runtime decodes the Input and works out what the Composition asks of
   it: it compiles the `compositionPolicy` (content-hash cached;
   malformed Cedar is a fatal result) and checks `limits` against its
   ceilings. A `module.from` source is then read from the
   observed composite resource (`type: OCI` with `from: status.module`
   expects `status.module` to be `{ref, credentials}`; a typo or a value
   of the wrong type is a fatal result that identifies the field) and
   fenced by the
   `compositionPolicy` (`pullModule` for its repository, `spendCredential`
   for a named credential - default-deny for XR-chosen sources). Resolving
   doesn't perform I/O: the **digest** that pins the module comes from the
   Input - the manifest digest of an OCI reference, `http.digest` for a
   URL - or from hashing a served file when it changes.
2. Where a signature is required, the runtime verifies it first, once per manifest
   digest per process and before any cache ([signatures](operators/signatures.md)).
   It then looks the digest up in the caches - compiled modules in memory (kept
   ten minutes after their last use), then Wasmtime artifacts on disk, then
   fetched modules on disk under `/tmp/function-wasm-cache`. Only a module
   never seen by this node is fetched (verified against its digest, written
   to disk) and compiled - about two seconds for a 75 MB Go module; a
   component that fails the world's typecheck, or a core module (ABI v1,
   removed in 1.0.0), is refused here - and its artifact written to disk.
   Restarts and registry outages don't need the network. Details in
   [docs/one-pager-cache.md](one-pager-cache.md). The module's
   [manifest](guests/module-manifests.md), if its artifact contains one, is then the
   module's ask: each `requires` capability must be permitted by the
   Input's `compositionPolicy` and the operator's `--sandbox-policy-file`
   (AND-combined - a manifest can only make a run fail earlier), and a
   `config` outside the module's schema is a fatal result before anything
   runs.
3. Each request gets a fresh instance and the
   caller's request bytes, less every step credential the three layers
   didn't grant (the pull credential always among them): WASI without
   network access, with only the filesystem and environment the three
   layers granted - a private `/tmp` created for this request and removed
   after it, exactly the environment variables its manifest binds to step
   credentials; HTTP requests, if granted, go through the host
   ([HTTP egress](operators/http-egress.md)); guest logs flow into the runtime's logger
   with the module reference attached; stdout and stderr are the pod's, so a
   Go panic's stack shows up in `kubectl logs`.
4. The response is returned as the module produced it, once the output
   fence has had its say where a policy layer scopes `composeResource`:
   every change the module made to the desired state must be permitted,
   or the step ends in a fatal result that identifies the kind, the name,
   the change and the layer ([Cedar policy reference](operators/cedar-policy.md#the-output-fence)).
   A trap, timeout
   (`limits.timeout` or `--module-timeout` - guest compute, with time spent
   waiting on the host's HTTP answers credited back - or the request
   deadline if sooner), memory limit (`limits.memory` or
   `--module-memory-limit`) or an unusable module is a fatal result that
   identifies the module - never a crashed function pod. A request that,
   with `--max-concurrent-runs` set, reaches its deadline while waiting
   for a run slot (`waiting for a run
   slot: deadline exceeded`) is a fatal result too: it never ran.

The full host/guest contract is in [`docs/abi-v2.md`](abi-v2.md).
