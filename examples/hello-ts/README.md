# hello-ts

A [Crossplane](https://crossplane.io) composition function in TypeScript,
componentized with [jco](https://github.com/bytecodealliance/jco) into an
**ABI v2** component ([docs/abi-v2.md](../../docs/abi-v2.md)) of about 14 MB
(SpiderMonkey embedded - TinyGo territory, not Rust territory) and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). It is the
same greeting function as every other example, and its `greetingUrl` fetch
is the platform's own `fetch()` - jco maps it to `wasi:http@0.2`, which the
runtime serves through the same egress policy, budgets and audit line as
every other guest's networking.

The toolchain is `npm install`: protobuf-es for the codec (generated
`js+dts` checked in - runtime JS plus full types; `make gen-proto` + protoc
to redo), `tsc --noEmit` as the type gate (node runs the tests by stripping
types natively), esbuild to bundle, jco to componentize. The official
[function-sdk-typescript](https://github.com/crossplane/function-sdk-typescript)
is deliberately not used: it targets the native deployment shape (a node
gRPC server), and its generated codec module itself imports @grpc/grpc-js,
which cannot exist inside a SpiderMonkey component. Two shapes worth knowing, both required by where
componentize-js is today:

- `run` is declared **sync** in this guest's wit (`wit/function.wit`):
  componentize-js cannot async-lift a custom world's export yet, and a
  sync-lifted function satisfies the runtime's async world. The JS still
  awaits freely - componentize-js resolves the promise before the export
  returns.
- the world's root-level `log` import arrives as a **default import from a
  module named after it** (`import log from "log"`, typed by
  `src/log.d.ts`), and esbuild must keep it external (`--external:log`).

This example is example-only: it passes the same behaviour tests as the
other guests (the guests suite through the real host, and
`make render-check`), but `guestfn init` does not scaffold it yet.

```shell
make build         # fn.wasm (bundle + componentize)
make test          # tsc --noEmit + native unit tests under node --test
make render        # serve this directory with the runtime and crossplane render
```
