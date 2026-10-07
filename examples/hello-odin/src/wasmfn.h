// The C half of the function-wasm ABI v2 glue of this guest: the c flavour's
// wasi:http@0.2 client (examples/hello-c/src/wasmfn.c) kept as it is, so the
// Odin half (src/wasmfn.odin) reaches the host's HTTP egress through one
// C call instead of driving wasi:http's resources and streams itself. The
// run export, the codec and the log import live on the Odin side.
#ifndef WASMFN_H
#define WASMFN_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// HTTP through the host (wasi:http/outgoing-handler). The guest never opens a
// socket: the host performs the request within the egress grant of the
// module's manifest and the operator's policy, or answers with a refusal.
typedef struct wasmfn_http_request {
	const char *method;         // NULL or "" is GET
	const char *url;            // absolute, http or https
	const char *const *headers; // alternating name/value C strings, NULL-terminated; NULL for none
	const uint8_t *body;        // NULL for none
	size_t body_len;
} wasmfn_http_request;

typedef struct wasmfn_http_response {
	int status;           // the server's status, whatever it is (a 503 is a response, not an error)
	const char **headers; // alternating name/value C strings, NULL-terminated, names lower-case
	uint8_t *body;        // body bytes, NUL-terminated for convenience
	size_t body_len;
} wasmfn_http_response;

// wasmfn_http_send performs req through the host. It returns true and fills
// rsp, or false and sets *err to the host's reason (refused by the grant or the
// policy, over a budget, failed: the error-code's internal-error string) or a
// guest-side problem talking to the host. Everything returned is heap
// allocated and never freed: a fresh instance serves each request.
bool wasmfn_http_send(const wasmfn_http_request *req, wasmfn_http_response *rsp, char **err);

// wasmfn_http_header returns the first value of the named response header
// (names compare case-insensitively), or NULL.
const char *wasmfn_http_header(const wasmfn_http_response *rsp, const char *name);

// wasmfn_http_get_text GETs url and returns the body of a 200 as a
// whitespace-trimmed C string, or NULL with *err set (a non-200 status is an
// error here). This is the call the Odin side makes.
char *wasmfn_http_get_text(const char *url, char **err);

// wasmfn_sprintf is printf into a heap-allocated string (NULL when out of
// memory).
char *wasmfn_sprintf(const char *format, ...);

#endif
