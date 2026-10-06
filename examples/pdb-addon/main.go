// Package main is the pdb-addon guest: a tenant add-on, compiled to
// WebAssembly and run by function-wasm, that gives every Deployment an
// earlier pipeline step composed a PodDisruptionBudget.
package main

import "github.com/jonasz-lasut/function-wasm/examples/pdb-addon/internal/wasmfn"

// Go runs package initializers, not main, in a wasip1 reactor
// (-buildmode=c-shared), so the function is registered from init.
func init() {
	wasmfn.Register(&Function{log: wasmfn.NewLogger()})
}

func main() {}
