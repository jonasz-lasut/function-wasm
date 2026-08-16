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

Export names and signatures are checked when a module is loaded; a mismatch
is reported once, at load, rather than on every request.

The host provides these imports:

| module | name | type | purpose |
|---|---|---|---|
| `wasi_snapshot_preview1` | * | | WASI preview 1 as implemented by wasmtime, with `argv = ["function"]`, no environment variables, no pre-opened directories, no sockets; the clock and randomness work; stdout and stderr are the host process's, so a guest's prints and panics land in the pod log |
| `wasmfn` | `log` | `(level: i32, ptr: i32, len: i32)` | structured logging through the host logger; optional |

A module importing anything else is refused at load.

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

## Compatibility

Payload evolution rides on protobuf: the host and the guest may be built
against different `function-sdk-go` versions, unknown fields survive a round
trip and new fields default. A change to the mechanics above (export names,
signatures, the packing) would be a new ABI with new export names; the host
would keep serving v1 modules.

## Examples

`examples/hello-go` (Go with `wasmfn` and function-sdk-go), `examples/hello-tinygo`
(TinyGo, protobuf-go types + vtprotobuf codecs) and `examples/hello-rust`
(Rust, prost) implement this contract for the same function — they are what
`guestfn init --lang go|tinygo|rust` scaffolds; the last two carry the ABI
glue in the open — about forty lines each — and are the reference for other
languages.

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
