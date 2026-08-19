//go:build generate

// Vendor crossplane's run_function.proto into the TinyGo, Rust, Zig and C
// templates and examples, copy the TinyGo, Zig and C examples' generated codecs
// into their templates, then regenerate the golden scaffolds under
// testdata/<lang>.
//go:generate go run vendorproto.go
//go:generate go test . -run ^TestRender$ -count=1 -update

package scaffold
