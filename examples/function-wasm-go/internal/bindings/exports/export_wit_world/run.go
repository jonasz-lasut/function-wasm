// Package export_wit_world holds the world-level export the generated
// wit_exports package calls: wit-bindgen's Go generator leaves an exported
// function to the application, as a function of this package. Run is a slot
// the wasmfn glue fills from its init, which keeps this package import-free
// and the import graph free of a cycle (wasmfn -> wit_exports -> here).
// Hand-written; wit-bindgen never touches it.
package export_wit_world

import witTypes "go.bytecodealliance.org/pkg/wit/types"

// Run implements the world's `run` export.
var Run func(request []byte) witTypes.Result[[]byte, string]
