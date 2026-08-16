# Local Loop and Run Diagnostics

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft

How a module author runs a module without a cluster, how a request that
failed in a cluster is replayed on a laptop, and what one run tells the
person reading the log. `function run` replays a `RunFunctionRequest`
through the real engine — `internal/engine`, `internal/sandbox`,
`internal/egress`, the pod's own code, no emulator; `guestfn dev` watches,
rebuilds and re-runs; `--capture-requests` writes requests the runtime saw,
credentials redacted, for `function run --request`; every run ends in one
structured summary line; a guest's stderr is captured per run and
attributed, its panic header lands next to the fatal result; traps carry
names when the module was built with `guestfn build --debug`. Nothing here
gates v0.1.0.


## Today

Running a module means `crossplane render` (Docker), a runtime process
serving a directory (`go run ./cmd/function --insecure --module-dir=.`, or
`examples/render.sh`) and a Composition plus an XR to build the request
from; nothing feeds the runtime a request directly, and nothing takes the
request that failed in a cluster to a laptop. When a Go guest panics the XR
condition says `module … failed: wasmfn_run failed: trap: unreachable code
reached (a Go guest prints the panic to stderr)`; the stack is in the pod's
log — stderr is inherited — interleaved with every other run's output and
attributed to nothing. The log carries `Running function` and, on failure,
`Request ended with a fatal result`: no per-phase timings, no memory
figure, no egress count. `guestfn build` strips (`-s -w`, TinyGo
`-no-debug`), so a trap's backtrace is `<wasm function 1827>`, one string
in the debug log.

## Goals and non-goals

Goals: one code path for cluster and laptop, so a local run is evidence
about the pod; `guestfn` stays pure Go; captured requests never carry a
credential value; a summary dashboards can be built on from logs alone (no
new metric labels — the digest rule stands); readable traps on demand. Not
goals: an emulator or second host implementation; replacing `crossplane
render`, which exercises Crossplane's own request building — `function run`
replays a request, `render` builds one; core dumps and the gdb stub
(wasmtime's Rust CLI only, not in the C API); capturing every request by
default; capturing stdout (`wasmfn.log` is the logging path).

## Shape

**`function run`** — a subcommand of the runtime binary (the `serve`/
`validate`/`run` split of the admission one-pager):

```shell
function run [fn.wasm] --request req.yaml|-            # a RunFunctionRequest (protojson, YAML or JSON): captured or hand-written
             [--xr xr.yaml]                            # instead of --request: a first-pass request built from an XR alone
             [--input input.yaml | --composition c.yaml [--step name]]   # the Input; default: the request's own
             [--enable-sandbox-* --sandbox-egress-policy p.yaml --module-timeout … --cosign-key … --module-dir …]  # the serve ceilings
             [--output yaml|json] [--summary text|json] [--watch] [--times N] [--debug-info]
```

The request comes from `--request` — a file `--capture-requests` wrote, or
one written by hand — or from `--xr`, which builds what a first reconcile
sends (the XR as the observed composite, nothing else; enough for the
scaffold's `example/xr.yaml`). The Input is the request's own `input`
unless `--input` or `--composition` replaces it; a bare `fn.wasm` argument
replaces only the Input's `module` with `{type: Path, path: fn.wasm}` and
serves its directory — rebuild, replay the cluster's request against the
new build, unchanged Composition. Then the seven steps of `RunFunction`
run as they would in the pod: admission, resolve, verify, load, run, the
same fatal results. The response is printed to stdout (YAML by default);
guest log lines (`wasmfn.log`), the guest's stderr and the egress audit
lines go to stderr as they are logged in the pod, prefixed by their kind;
the summary line closes the run, human-readable or as one JSON object.
Exit codes: `0` the response carries no fatal result; `1` it does — from
the guest or the host, printed either way — as `crossplane render` would
have shown a failure; `2` the tool itself failed (unreadable input, bad
flags). `--watch` re-runs when the module file's size or mtime changes
(what `path` sources hash on; a 250 ms poll, no watcher dependency);
`--times N` runs the request N times and prints p50/p95 per phase — the
number an author wants before choosing between Go, TinyGo and Rust;
`--debug-info` is below.

**`guestfn serve` and `guestfn dev`.** `guestfn serve` is
`examples/render.sh` without the shell: the runtime serving the project
directory (`--insecure --module-dir=.`) so `crossplane render` with the
scaffold's Development `functions.yaml` works. `guestfn dev` watches the
project's sources, runs `guestfn build` on a change (post-build ABI check
included) and then either replays — `--request req.yaml` becomes `function
run fn.wasm --request req.yaml`, printed each time — or renders: the runtime
keeps serving between changes (a rebuilt `fn.wasm` is a new size and mtime,
so a new digest and a recompile, no restart) and `crossplane render
example/xr.yaml example/composition.yaml example/functions.yaml
--include-function-results` runs after each build. The runtime is
`function` on `PATH`, `--runtime <path>`, or the package image
(`--runtime-image`; `docker run --rm -v "$PWD":/w -p 9443:9443
ghcr.io/jonasz-lasut/function-wasm:vX.Y.Z …` — the package image is the
runtime image); a released `guestfn` defaults the tag to its own version,
the lockstep tags, and a `(devel)` build requires `--runtime`. `guestfn`
execs, it does not link: pure Go, as it runs `go`, `tinygo`, `cargo` and
`wasm-opt` today.

**`--capture-requests <dir>`** on `function serve`, off by default, writes
each `RunFunctionRequest` the runtime handled to a file, `--capture-when
fatal|all` (default `fatal`: requests that ended in a fatal result, host or
guest), `--capture-digest sha256:…` (repeatable: only these modules),
`--capture-max-files` (default 100, the oldest removed). One file per
request, `<UTC time>_<digest, 12 hex>_<outcome>.yaml`, a `RunFunctionRequest`
in protojson exactly — `function run --request` takes it verbatim — plus a
`.meta.yaml` sibling with the time, module reference and digest, outcome
and reason, runtime version and what was redacted; files `0600` in a
directory created `0700`. Redaction, always, no flag to disable: every
value under `credentials[*].credential_data.data` replaced by the string
`REDACTED` (keys kept, so a replay sees which credentials the step had);
`observed.composite.connection_details` and
`observed.resources[*].connection_details` values likewise; `data` and
`stringData` of any resource of `kind: Secret` under `observed`, `desired`
and `extra_resources`. `input` (with `config` and `sandbox.env`, non-secret
by the Input's own convention), `context` and the rest of the observed and
desired state are written as they are — they are the thing to replay, and
they are cluster state the operator who set the flag can read anyway. The
runtime logs `Captured request` with the path; the metric is not touched.

**One summary line per run.** `Ran module` at info, replacing `Running
function` (which moves to debug) and folding `Request ended with a fatal
result` into it: `outcome` (`ok`, `refused`, `error`, `timeout`), `tag`,
`module`, `digest`, `composite` (apiVersion, kind and name of the observed
XR), `resolve_ms`, `verify_ms`, `load_ms` with `load` (`memory`, `disk`,
`compile`), `instantiate_ms` (instantiate and `_initialize`), `run_ms`
(`wasmfn_run`), `request_bytes`, `response_bytes`, `memory_bytes` (the
linear memory's size when the store closes — a high-water mark, wasm
memory never shrinks), `egress_requests` and `egress_bytes`,
`guest_log_lines`, `guest_stderr_bytes`, `results` (fatal, warning, normal
counts) and `reason` when the outcome is not `ok`. `engine.Run` reports
`Stats{Instantiate, Initialize, Run time.Duration; MemoryBytes; HTTP
requests and bytes; StderrBytes}` next to the response. Guest-log hygiene
comes with it: `--max-guest-log-bytes` (default 64 KiB per run, `0`
unbounded) budgets `wasmfn.log` on the run; past it one `Guest log
truncated` line and the rest is dropped, so a looping guest cannot flood
the log pipeline.

**Guest stderr per run.** Each run's store gets its own stderr file
(`WasiConfig.SetStderrFile` — the one alternative to inheriting the pod's
descriptors wasmtime-go v47 offers; the C API's stderr callback is not
wrapped): `os.CreateTemp` under `os.TempDir()` before the store, handed to
wasmtime by path, removed after the store on every path out of `Run` — the
private `/tmp`'s discipline. Empty afterwards (the common case) it costs
nothing more; otherwise its last `--max-guest-stderr-bytes` (64 KiB) are
logged as `Guest stderr` with `module`, `digest`, `tag`, `bytes`,
`truncated` — at info when the run failed, at debug when it succeeded, so
a chatty guest does not promote itself. On a fatal outcome the **panic
header** — the first line starting with `panic:` (Go, TinyGo) or `thread
'…' panicked at` plus the line after it (Rust), else the first non-empty
line, at most 256 bytes — is appended to the fatal result: `module oci …
failed: wasmfn_run failed: trap: unreachable code reached; guest stderr:
panic: runtime error: index out of range [3] with length 2`, replacing
today's "(a Go guest prints the panic to stderr)" hint, so `crossplane
render --include-function-results` and the XR condition say *why*.
`--capture-guest-stderr` (default true; off is today's `InheritStderr`) and
`--attach-guest-panic` (default true) are the knobs. Cost: a create, a stat
and a remove per run — tens of microseconds against a Go guest's 8–11 ms,
comparable to a Rust guest's 50 µs — measured in phase 2 and stated in the
README's sizing table; the flag is for runtimes serving microsecond-class
modules at high rate. stdout stays inherited.

**Readable traps.** `guestfn build --debug` keeps the `name` section and
DWARF: Go without `-s -w`, TinyGo without `-no-debug`, Rust with
`CARGO_PROFILE_RELEASE_DEBUG=true` and no strip; the module grows and is
for a laptop or a debugging deployment, not a release. In the engine,
`guestError` logs `Trap.Frames()` — `FuncName` (or `#<index>` for a
stripped module) and `ModuleOffset`, at most 32 frames — as one structured
field at debug level next to the trap message it already logs; `function
run` prints them always. `--debug-info` (`serve` and `run`) sets
`Config.SetDebugInfo(true)`, so wasmtime's own backtrace text carries
`file:line` from a `--debug` build's DWARF, at a compile-time and
artifact-size cost; the compiled store's version directory gets a
`-debuginfo` suffix so the two configurations' artifacts never mix. Off by
default; `guestfn inspect` says whether a module has names and DWARF at all.

## Mechanics

- `cmd/function/main.go`: subcommands (`serve` default-with-args, `run`,
  `validate`) over one embedded ceilings struct; new flags
  `--capture-requests`, `--capture-when`, `--capture-digest`,
  `--capture-max-files`, `--capture-guest-stderr`, `--attach-guest-panic`,
  `--max-guest-log-bytes`, `--max-guest-stderr-bytes`, `--debug-info`.
- `cmd/function/run.go` (+ `run_test.go`: goldens over `testwasm.Fixed`
  modules — a response, a guest fatal, a trap with frames, `--summary
  json`, `--xr`, the `fn.wasm` override — and, outside `-short`, the three
  example guests replaying a captured request); `cmd/function/capture.go`
  (`capture`, `redact`; a table test with credentials, connection details
  and a Secret extra resource → golden, file mode asserted);
  `cmd/function/fn.go` (`runSummary` through the seven steps, the capture
  hook after `fatal` and after `Run`).
- `internal/engine/run.go` (`Stats`, the stderr file, `Trap.Frames()` in
  `guestError`), `sandbox.go` (`stderrFile`/`removeStderrFile` next to
  `privateTmp`), `hostlog.go` (the byte budget on `call`), `engine.go`
  (`Config.DebugInfo`, the `Version()` suffix); a `testwasm` fixture that
  writes to fd 2 through `fd_write` (the `wasi.go` pattern) and traps pins
  the `Guest stderr` line, the panic header and the removed file; `TestRun`
  cases for the log budget and `Stats`.
- `cmd/guestfn/dev.go` (`DevCmd`, `ServeCmd`; `fsnotify` for the watch),
  `main.go` (`build --debug`, checked in `main_test.go` with
  `wasmbin.HasNames`).
- Docs: `docs/abi.md` (the WASI row: stderr captured per run and
  attributed; the panic header may be attached to the fatal result), README
  ("Render locally" becomes "Run locally"; "Runtime flags"; troubleshooting),
  the resource-governance one-pager (log budgets in the table).

## Trust and threat notes

Captured requests hold cluster state — that is their purpose — and never a
credential value, connection detail or Secret datum: redaction is
unconditional, files are `0600` in a `0700` directory the operator chose,
the count is bounded, the flag is off by default and documented as a
debugging aid for a volume with the pod log's access rules; nothing is
captured over the network. The panic header attached to a result is text
the module wrote — the same class as a fatal `Result` a guest returns on
purpose, module-authored and Composition-trusted — which is why the default
is on; the rest of stderr stays in the log, bounded, and
`--attach-guest-panic=false` exists for runtimes whose XR authors are less
trusted than the module authors. Trap frames name guest functions in the
debug log only. `function run` on a laptop applies the pod's sandbox — no
host mounts, egress and the private `/tmp` only behind the same flags, off
by default — so replaying a captured request performs no request the
Composition and the local flags do not allow. `--debug` builds carry
source paths and symbol names: publish release builds.

## Phasing

| phase | what | effort | release |
|---|---|---|---|
| 1 | `guestfn build --debug`, `Trap.Frames()` in the debug log, `--debug-info` with the versioned artifact directory | S | v0.2 |
| 2 | per-run stderr file, `Guest stderr` line, panic header on the fatal result, `--max-guest-log-bytes` | S–M | v0.2 |
| 3 | `engine.Stats` and the `Ran module` summary line | S | v0.2 |
| 4 | `function run` (`--request`, `--xr`, the `fn.wasm` override, `--watch`, `--times`) on the subcommand split | M | v0.2 |
| 5 | `guestfn serve`, `guestfn dev` (render and replay modes, `--runtime`/`--runtime-image`) | S–M | v0.2–v0.3 |
| 6 | `--capture-requests` with redaction and the `.meta.yaml`; `guestfn dev --request` closes the loop | M | v0.3 |

None of it touches the Input or the ABI's exports; the summary line
replaces two log lines, stderr handling is behaviour, not contract. Nothing
here gates v0.1.0.

## Decisions for Jonasz

- **Stderr: capture always, or only under `--debug`?** (open question 6)
  Recommended: always (`--capture-guest-stderr` default true). A production
  panic is exactly the case that needs the stack attributed to a module
  and a tag, and `--debug` is a logging verbosity flag nobody runs in
  production; the cost is a create/stat/remove per run, measured in phase
  2, and the flag turns it off where microseconds matter.
- **Attach the panic header to the fatal result, or keep it in the log?**
  Recommended: attach, default on (`--attach-guest-panic`). It is
  module-authored text of the same class as a fatal `Result`; one line,
  256 bytes; the operator can turn it off.
- **Where `function run` lives** (open question 1, shared with the
  admission one-pager): the runtime binary; `guestfn dev` execs it or the
  package image. Recommended: yes — `guestfn` stays installable without a
  C compiler for Rust and TinyGo authors, and a local run is the pod's own
  code path by construction.
- **`--capture-when` default `fatal`.** Capturing every request for a
  digest is for reproducing a wrong-but-successful output; failures are the
  common need. Recommended: `fatal` default, `all` opt-in with
  `--capture-digest` strongly suggested in the flag's help.
- **stdout.** Leave it inherited; a guest that prints should use
  `wasmfn.log`. Recommended: yes for now — capturing it too doubles the
  per-run file cost for a stream Go, TinyGo and Rust guests rarely use.
