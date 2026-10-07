# How a request runs

1. The Input is decoded, and what the Composition asks of the runtime is
   settled: the `compositionPolicy` is compiled (content-hash cached;
   malformed Cedar is a fatal result), `limits` are checked against the
   runtime's ceilings. A `module.from` source is then read from the
   observed composite resource (`type: OCI` with `from: status.module`
   expects `status.module` to be `{ref, credentials}`; a typo or a wrong
   shape is a fatal result naming the field) and fenced by the
   `compositionPolicy` (`pullModule` for its repository, `spendCredential`
   for a named credential - default-deny for XR-chosen sources). Resolving
   does no I/O: the **digest** that pins the module comes from the Input —
   the manifest digest of an OCI reference, `http.digest` for a URL — or
   from hashing a served file when it changes.
2. The digest is looked up in the caches — compiled modules in memory (kept
   ten minutes after their last use), then wasmtime artifacts on disk, then
   fetched modules on disk under `/tmp/function-wasm-cache`. Only a module
   never seen by this node is fetched (verified against its digest, written
   to disk) and compiled - about two seconds for a 75 MB Go module; a
   component that does not implement the world, or a core module (ABI v1,
   removed in 1.0.0), is refused here - and its artifact written to disk.
   Restarts and registry outages need no network. Details in
   [docs/one-pager-cache.md](one-pager-cache.md). The module's
   [manifest](guests/module-manifests.md), if its artifact carries one, is then the
   module's ask: each `requires` capability must be permitted by the
   Input's `compositionPolicy` and the operator's `--sandbox-policy-file`
   (AND-combined - a manifest can only make a run fail earlier), and a
   `config` outside the module's schema is a fatal result before anything
   runs.
3. Every request gets a fresh instance (about ten milliseconds) and the
   caller's request bytes, less every step credential the three layers did
   not grant (the pull credential always among them): WASI with no
   network access, and no filesystem or environment beyond what the three
   layers granted - a private `/tmp` created for this request and removed
   after it, exactly the environment variables its manifest binds to step
   credentials; HTTP requests, if granted, go through the host
   ([HTTP egress](operators/http-egress.md)); guest logs flow into the runtime's logger
   with the module reference attached; stdout and stderr are the pod's, so a
   Go panic's stack shows up in `kubectl logs`.
4. The response is returned as the module produced it. A trap, timeout
   (`limits.timeout` or `--module-timeout` - guest compute, with time spent
   waiting on the host's HTTP answers credited back - or the request
   deadline if sooner), memory limit (`limits.memory` or
   `--module-memory-limit`) or an unusable module is a fatal result naming
   the module - never a crashed function pod. So is a request that, with
   `--max-concurrent-runs` set,
   reaches its deadline while waiting for a run slot (`waiting for a run
   slot: context deadline exceeded`): it never ran.

The full host/guest contract is in [docs/abi-v2.md](abi-v2.md).
