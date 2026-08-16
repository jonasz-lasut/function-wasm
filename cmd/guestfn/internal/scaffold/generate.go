//go:build generate

// Vendor crossplane's run_function.proto into the TinyGo and Rust templates
// and examples, copy the TinyGo example's generated codecs into its template,
// then regenerate the golden scaffolds under testdata/<lang>.
//go:generate go run vendorproto.go
//go:generate go test . -run ^TestRender$ -count=1 -update

package scaffold
