# function-wasm guest ABI v1

This is the contract between the function-wasm runtime (the *host*) and a
WebAssembly module it runs (the *guest*). It is deliberately small so a guest
can be written in any language with a wasip1 toolchain; the Go `wasmfn`
package implements it for Go guests.

## Module shape

A guest is a WebAssembly module targeting **WASI preview 1** (`wasip1`). It
must export:

| export | type | purpose |
|---|---|---|
| `memory` | memory | the linear memory both sides read and write |
| `wasmfn_alloc` | `(size: i32) -> i32` | allocate `size` bytes and return their address; the guest keeps them valid until the next `wasmfn_run` returns |
| `wasmfn_run` | `(ptr: i32, len: i32) -> i64` | run the function; see below |
| `_initialize` | `() -> ()` | optional; if present the host calls it once per instance before anything else (WASI reactor convention — Go's `-buildmode=c-shared` emits it) |

Export names and signatures are checked when a module is loaded — from
wasmtime's own decoded types, once it is compiled; a mismatch is reported
once, at load, rather than on every request. `guestfn build` prints the same
verdict (it compiles the module with the same engine), `guestfn push`
refuses a module that fails it, and `guestfn inspect fn.wasm` shows exactly
what the runtime sees: exports, imports and memories with their types.

The host provides these imports:

| module | name | type | purpose |
|---|---|---|---|
| `wasi_snapshot_preview1` | * | | WASI preview 1 as implemented by wasmtime, with `argv = ["function"]`, no sockets; no environment variables and no pre-opened directories unless the module's manifest requires them and the policy layers permit (see [Sandbox](#sandbox)); the clock and randomness work; stdout and stderr are the host process's, so a guest's prints and panics land in the pod log |
| `wasmfn` | `log` | `(level: i32, ptr: i32, len: i32)` | structured logging through the host logger; optional |
| `wasmfn` | `http` | `(req_ptr: i32, req_len: i32) -> i64` | one HTTP(S) request performed by the host within the egress grant of the module's manifest; optional — see [HTTP egress](#http-egress) |

A module importing anything else is refused at load. Both `wasmfn` imports
are always provided and type-checked at load; a module imports the ones it
uses. Whether a `wasmfn.http` call is *performed* is decided per request by
the grant, never at load.

## One request

1. The host instantiates the module in a fresh store and calls `_initialize`
   if exported.
2. The host encodes the `RunFunctionRequest` in protobuf wire format
   (message `apiextensions.fn.proto.v1.RunFunctionRequest`, from
   `crossplane/function-sdk-go/proto/v1/run_function.proto`), calls
   `wasmfn_alloc(len)` and copies the bytes to the returned address.
3. The host calls `wasmfn_run(ptr, len)` with that address and length. **The
   request always lives in a buffer obtained from `wasmfn_alloc`**, so a
   guest may look the buffer up rather than trust the pointer.
4. `wasmfn_run` returns an `i64` packing the address and length of the
   response: `(ptr << 32) | len`. The bytes are a protobuf-encoded
   `apiextensions.fn.proto.v1.RunFunctionResponse`. `0` means an empty
   response. The buffer must stay valid until the call returns; the host
   copies it out immediately.
5. The host drops the store. Nothing survives between requests — no globals,
   no heap, no open handles — so a guest never has to be reentrant or clean up.

The whole request is forwarded (input, observed and desired state, context,
credentials, required resources and schemas) and the whole response is
returned to Crossplane unchanged (desired state, results, conditions,
requirements, context, output, TTL). A guest is a complete composition
function; the host adds nothing to its response except a `meta` block when
the guest omitted one.

## Errors

| what | how the guest reports it | what Crossplane sees |
|---|---|---|
| a function-level failure (bad input, unusable resource, …) | a fatal `Result` in the response, exactly like a native function | a fatal result |
| the Go guest's `RunFunction` returns an `error` | `wasmfn` turns it into a fatal result on a fresh response — the same outcome a gRPC error has for a native function | a fatal result |
| the Go guest panics | `wasmfn` recovers it into a fatal result and prints the stack to stderr | a fatal result, stack in the pod log |
| a trap (`unreachable`, out-of-bounds access, stack overflow), a WASI `proc_exit`, the execution deadline, the memory limit | the instance dies | the host returns a fatal result naming the module and the cause; wasmtime's backtrace goes to the debug log |
| the module cannot be fetched, verified, compiled or lacks the exports | — | a fatal result at load |

## Logging

`wasmfn.log(level, ptr, len)` takes a UTF-8 JSON object at `ptr`/`len`:

```json
{"msg": "Running function", "kv": ["tag", "abc123", "count", 3]}
```

`kv` is an even-length list of alternating keys and values, as
`logging.Logger` takes them. `level` 0 is info, 1 is debug. The host logs the
record through its own logger with the module reference and digest attached.
A malformed record is logged verbatim rather than dropped.

## Sandbox

By default a guest sees no filesystem and no environment. A module may be
granted some by declaring them in its manifest (`requires`; the policy
layers must permit - `docs/one-pager-three-layer-authz.md`,
`docs/one-pager-sandbox.md`), and the grant reaches the guest through WASI
alone — no new import, nothing language-specific:

| granted requirement | what the guest sees |
|---|---|
| `requires.filesystem.privateTmp: true` | an empty, writable directory pre-opened at `/tmp` — where Go's `os.TempDir()` and Rust's `env::temp_dir()` point on WASI — created for this request and removed after it, whatever the outcome. Nothing written there survives to the next request or is visible to another. It is the only directory a guest is ever given: host directories are not mountable, a path that would leave `/tmp` (`/tmp/../etc/passwd`) never reaches the host filesystem — wasmtime resolves paths inside the pre-open and answers `EPERM` — and language runtimes that clean absolute paths against the pre-opens fail even earlier (Go: `EBADF`, no pre-open matches `/etc/passwd`) |
| `requires.env` credential bindings | exactly the bound variables, resolved from the pipeline step's credentials, through `environ_sizes_get`/`environ_get` (`os.Getenv`, `std::env::var`); the host's environment is never inherited |

The private `/tmp` is pre-opened as descriptor 3, but that number is not
part of the contract: a guest that talks to WASI directly should discover
pre-opens with `fd_prestat_get` / `fd_prestat_dir_name`, as language
runtimes do. Nothing else changes: the same request/response exchange, the
same deadline and memory limit.

## HTTP egress

`wasmfn.http(req_ptr, req_len) -> i64` asks the host to perform one HTTP(S)
request. The guest never opens a socket: the host resolves the name, refuses
addresses its policy blocks, terminates TLS with its own roots, checks the
host, method and path against the `requires.egress.http` rules of the
module's manifest that the policy layers granted, follows redirects within
them, enforces the operator's budgets and returns the response. The import
exists on every runtime; without a grant every call is answered with a
refusal — never a trap.

Memory protocol, in the order it happens:

1. The guest writes a UTF-8 JSON **request** anywhere in its memory and calls
   `wasmfn.http(ptr, len)`. The host copies it out at once — nothing the host
   does later reads that buffer again.
2. The host performs (or refuses) the request, then calls the guest's own
   `wasmfn_alloc(n)` for the JSON **response** of `n` bytes, writes it there
   and returns `(ptr << 32) | n`. **`wasmfn_alloc` is therefore called
   re-entrantly, while `wasmfn_run` and `wasmfn.http` are on the stack**; it
   must be callable then (a bump allocator, a Go `//go:wasmexport`, a Rust
   `Vec` all are), and it may grow the memory — the host re-reads the memory
   base after the call. Like the request, the response is in a
   `wasmfn_alloc` buffer, so a guest may look it up rather than trust the
   pointer.
3. A trap or exit *inside* the re-entered `wasmfn_alloc`, or an invalid
   buffer from it, ends the run like any other trap. `0` is reserved for "no
   response" — a guest treats it as an error; the host does not return it
   today.

The request:

```json
{"method": "POST", "url": "https://api.example.com/v1/items?limit=1",
 "headers": {"Accept": ["application/json"]}, "body": "eyJuYW1lIjoieCJ9"}
```

`method` defaults to `GET`; `url` is absolute, `http` or `https`; `headers`
maps names to lists of values (`Host`, `Content-Length` and hop-by-hop
headers are dropped — the host owns the connection; the host adds its own
`User-Agent` and `Accept-Encoding: gzip`, transparently decoded, unless the
guest sets them); `body` is base64, optional — the standard alphabet with
padding (RFC 4648 §4), no line breaks, in both directions.

The response:

```json
{"status": 200, "headers": {"Content-Type": ["application/json"]}, "body": "eyJvayI6dHJ1ZX0="}
{"status": 0, "error": "sandbox.egress: no rule admits host \"evil.example.com\""}
```

`status` is the server's status code, whatever it is (a 503 is a response,
not an error), with the server's `headers` (names canonicalised as Go's
`net/http` does: `Content-Type`) and base64 `body`; `status` 0 with an
`error` means the host did not perform the request: it was refused by the
grant or the operator's policy (`sandbox.egress: …` — the guest is told that
the policy refused, not which address or block-list entry did; that stays in
the operator's audit line), a budget was hit (request count, response bytes,
response headers, redirects, the request timeout), the payload was
malformed, or the transport failed (DNS, TLS, connection). A request is also
cut short at the run's deadline; the run itself then ends as a timeout, so
the guest does not get to act on that error. Redirects are followed within
the grant, each hop re-checked and audited; as in Go's `net/http`,
`Authorization` and `Cookie` headers survive a redirect to the same host and
are dropped on a redirect elsewhere. A refused request is never a trap.

The Go SDK's `wasmfn.HTTPClient()` returns an `*http.Client` whose transport
speaks this protocol; the TinyGo and Rust scaffolds carry `http.go` and
`src/http.rs` — the same protocol over their own allocator export, about a
hundred lines each — the reference for other languages.

## Compatibility

Payload evolution rides on protobuf: the host and the guest may be built
against different `function-sdk-go` versions, unknown fields survive a round
trip and new fields default. A change to the mechanics above (export names,
signatures, the packing) would be a new ABI with new export names; the host
would keep serving v1 modules. Host imports are added within ABI v1 as
optional imports (`wasmfn.http` was): a module that does not import one is
unaffected, and their JSON payloads may gain fields.

## Examples

`examples/hello-go` (Go with `wasmfn` and function-sdk-go), `examples/hello-tinygo`
(TinyGo, protobuf-go types + vtprotobuf codecs) and `examples/hello-rust`
(Rust, prost) implement this contract for the same function — they are what
`guestfn init --lang go|tinygo|rust` scaffolds; the last two carry the ABI
glue in the open — about forty lines each, plus the `wasmfn.http` helper —
and are the reference for other languages.

## Go guests

`wasmfn` (module `github.com/jonasz-lasut/function-wasm/pkg/wasmfn`) implements
the exports and the logger. A guest registers its function from `init`
(Go never runs `main` in a wasip1 reactor) and builds with

```shell
GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -trimpath -ldflags "-s -w" -o fn.wasm .
```

or `guestfn build`. Expect ~75 MB (13 MB compressed) for a guest that uses
`function-sdk-go`'s `request`/`response`/`resource` packages: they pull in
crossplane-runtime and Kubernetes apimachinery, as a native function binary
does. A guest that works on the raw protobuf messages (`fnv1` + `structpb`)
through `wasmfn` alone is ~20 MB. Compilation by the host takes about two
seconds and is cached by content digest; a request then costs about ten
milliseconds.
