// Package wasmfn turns an ordinary function-sdk-go RunFunction into a
// WebAssembly guest for function-wasm.
//
// A guest is a wasip1 reactor built with
//
//	GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -o fn.wasm .
//
// Go never runs main in that build mode: package initializers run when the
// host calls _initialize, so the guest registers its function from init:
//
//	func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger()}) }
//	func main()  {}
//
// Register and NewLogger are portable so the guest package still builds and its
// tests still run natively; only the ABI exports and the host log import are
// wasip1-specific. See docs/abi.md in the function-wasm repository for the
// host/guest contract (exports wasmfn_alloc and wasmfn_run, protobuf payloads).
//
// The package deliberately imports only function-sdk-go's proto types and
// crossplane-runtime's logging interface: a guest that speaks raw protobuf
// stays small (about 15 MB), while one using function-sdk-go's request,
// response and resource packages inherits their Kubernetes dependencies
// (about 75 MB), exactly as a native function binary does.
package wasmfn
