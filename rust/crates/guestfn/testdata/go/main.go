// Package main is the my-fn guest: a composition function compiled to
// WebAssembly and run by function-wasm.
package main

import "github.com/example/my-fn/internal/wasmfn"

// Go runs package initializers, not main, in a wasip1 reactor
// (-buildmode=c-shared), so the function is registered from init.
func init() {
	wasmfn.Register(&Function{log: wasmfn.NewLogger(), http: wasmfn.HTTPClient()})
}

func main() {}
