module github.com/example/my-fn

go 1.26.6

// A floor for function-sdk-go v0.7.1's grpc v1.81, whose transport does not
// build on Go 1.27 (golang.org/x/net/http2 moves into the standard library
// there). Drop it once a function-sdk-go release requires grpc v1.83 or newer.
require google.golang.org/grpc v1.83.2

// The canonical-ABI runtime the bindings under internal/bindings call, at
// the version wit-bindgen 0.62.0's Go generator targets (wit-bindgen go
// --print-remote-pkg-version); moves with the bindings, not on its own.
require go.bytecodealliance.org/pkg v0.2.3

require github.com/crossplane/function-sdk-go v0.7.1
