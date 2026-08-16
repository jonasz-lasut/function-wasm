// Package wire holds the JSON payloads of the wasmfn.http import — the
// contract between a guest and the host, documented in docs/abi.md — and
// nothing else, so the engine that serves the import and the policy that
// answers it share the types without the engine depending on the policy.
package wire

// Request is the JSON payload a guest hands to wasmfn.http.
type Request struct {
	// Method of the request; empty means GET.
	Method string `json:"method,omitempty"`
	// URL, http or https, absolute.
	URL string `json:"url"`
	// Headers to send. Host, Content-Length and hop-by-hop headers are the
	// host's to set and are dropped.
	Headers map[string][]string `json:"headers,omitempty"`
	// Body bytes (base64 on the wire, as encoding/json renders []byte).
	Body []byte `json:"body,omitempty"`
}

// Response is the JSON payload wasmfn.http returns. A request that was not
// performed — refused by the grant or the policy, over a budget, or failed —
// has Status 0 and an Error; a response from the server has its status,
// whatever it is, and no Error.
type Response struct {
	Status  int                 `json:"status"`
	Headers map[string][]string `json:"headers,omitempty"`
	Body    []byte              `json:"body,omitempty"`
	Error   string              `json:"error,omitempty"`
}
