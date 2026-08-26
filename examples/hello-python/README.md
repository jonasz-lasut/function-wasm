# hello-python

A [Crossplane](https://crossplane.io) composition function in Python,
componentized with
[componentize-py](https://github.com/bytecodealliance/componentize-py) into
an **ABI v2** component ([docs/abi-v2.md](../../docs/abi-v2.md)) of about
21 MB (CPython embedded) and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). It is the
same greeting function as every other example; its `greetingUrl` fetch goes
through `wasi:http@0.2`'s `outgoing-handler` on componentize-py's poll
loop, which the runtime serves through the same egress policy, budgets and
audit line as every other guest's networking.

The toolchain is `python3` alone: a venv with `componentize-py` and the
pure-Python `protobuf` runtime (the C extension does not exist in wasm; the
fallback engages by itself). Like the TypeScript guest, `run` is declared
sync in this guest's wit - a sync-lifted function satisfies the runtime's
async world.

This example is example-only: it passes the same behaviour tests as the
other guests (the guests suite through the real host, and
`make render-check`), and `guestfn build` builds it (`requirements.txt`
detection, or `--lang python`), but `guestfn init` does not scaffold it
yet.

- `proto/run_function.proto` is crossplane's `RunFunction` contract,
  vendored; the checked-in `src/gen/run_function_pb2.py` is protoc's
  output (`make gen-proto` to redo)
- `wit/function.wit` is this guest's world - the wasmfn contract plus the
  `wasi:http` import it fetches with; `wit/deps/` carries the wasi 0.2 WIT
  that import needs
- `src/fn.py` is the function - ordinary Python over the protobuf
  messages, natively testable (`make test`) with a fetch double
- `src/app.py` (wasm only at runtime) is the world wiring: the `run`
  export, the typed log adapter and the poll-loop fetch

```shell
make build         # fn.wasm (a component; python3 + venv)
make test          # native unit tests under unittest
make render        # serve this directory with the runtime and crossplane render
```
