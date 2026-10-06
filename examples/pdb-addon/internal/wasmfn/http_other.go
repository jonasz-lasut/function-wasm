//go:build !wasip1

package wasmfn

// hostHTTPCall has no host to ask outside a wasip1 build; native tests of a
// guest see ErrNoHostHTTP unless they replace httpCall.
func hostHTTPCall([]byte) ([]byte, error) {
	return nil, ErrNoHostHTTP
}
