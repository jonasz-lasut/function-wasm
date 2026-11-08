// Package wasmfn turns an ordinary function-sdk-go RunFunction into a
// WebAssembly component for function-wasm (ABI v2).
//
// A guest is a wasip1 reactor built with
//
//	GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -ldflags=-checklinkname=0 -o fn.wasm .
//
// that guestfn build wraps into a component: it embeds the world (wit/) and
// links the wasip1 adapter of the runtime's own wasmtime, so the preview1
// imports Go emits become the WASI 0.2 interfaces the runtime serves. Go
// never runs main in that build mode: package initializers run when the
// host instantiates the component, so the guest registers its function from
// init:
//
//	func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger()}) }
//	func main()  {}
//
// Register, NewLogger, GetConfig and HTTPClient are portable so the guest
// package still builds and its tests still run natively; only the world
// wiring is wasip1-specific (host_wasip2.go): the run export's slot, the
// typed log import and wasi:http@0.2's outgoing-handler behind HTTPClient,
// reached through the bindings wit-bindgen's Go generator writes under
// internal/bindings. See docs/abi-v2.md in the function-wasm repository for
// the host/guest contract.
//
// The package deliberately imports only function-sdk-go's proto types and
// crossplane-runtime's logging interface: a guest that speaks raw protobuf
// stays small (about 20 MB), while one using function-sdk-go's request,
// response and resource packages inherits their Kubernetes dependencies
// (about 75 MB), exactly as a native function binary does.
package wasmfn
