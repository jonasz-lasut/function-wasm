// The function-wasm ABI glue for a C guest (docs/abi.md): the wasmfn_alloc /
// wasmfn_run exports and the handle behind them (decode the request, run the
// function, encode the response; every failure becomes a fatal result), the
// wasmfn.log import as a logger and the wasmfn.http import as an HTTP client.
// Only the exports and the two imports are wasi-specific; natively the logger
// prints to stderr and a test may install a fake host for wasmfn.http, so the
// function builds and tests as an ordinary C program.
#ifndef WASMFN_H
#define WASMFN_H

#include <cJSON.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// wasmfn_handle is the guest half of ABI v1: it decodes a RunFunctionRequest
// from in, calls run_function (fn.h) and returns the encoded
// RunFunctionResponse (heap allocated, *out_len bytes). A request that cannot
// be decoded and a function that fails are fatal results, so the host can
// always decode the reply.
uint8_t *wasmfn_handle(const uint8_t *in, size_t in_len, size_t *out_len);

// Logging through the host's logger (wasmfn.log). kv are alternating key/value
// C strings, terminated by NULL:
//
//	wasmfn_log_info("Running function", "tag", tag, NULL);
void wasmfn_log_info(const char *msg, ...);
void wasmfn_log_debug(const char *msg, ...);

// HTTP through the host (wasmfn.http). The guest never opens a socket: the host
// performs the request within the egress grant of the module's manifest and
// the operator's policy, or answers with a refusal.
typedef struct wasmfn_http_request {
	const char *method;        // NULL or "" is GET
	const char *url;           // absolute, http or https
	const char *const *headers; // alternating name/value C strings, NULL-terminated; NULL for none
	const uint8_t *body;       // NULL for none
	size_t body_len;
} wasmfn_http_request;

typedef struct wasmfn_http_response {
	int status;      // the server's status, whatever it is (a 503 is a response, not an error)
	cJSON *headers;  // the response headers as the host sent them: {"Content-Type": ["text/plain"]}
	uint8_t *body;   // decoded body bytes, NUL-terminated for convenience
	size_t body_len;
} wasmfn_http_response;

// wasmfn_http_send performs req through the host. It returns true and fills
// rsp, or false and sets *err to the host's reason (refused by the grant or the
// policy, over a budget, failed) or a guest-side problem talking to the host.
// Everything returned is heap allocated and never freed: a fresh instance
// serves each request.
bool wasmfn_http_send(const wasmfn_http_request *req, wasmfn_http_response *rsp, char **err);

// wasmfn_http_header returns the first value of the named response header
// (names compare case-insensitively), or NULL.
const char *wasmfn_http_header(const wasmfn_http_response *rsp, const char *name);

// wasmfn_http_get_text GETs url and returns the body of a 200 as a
// whitespace-trimmed C string, or NULL with *err set (a non-200 status is an
// error here).
char *wasmfn_http_get_text(const char *url, char **err);

// wasmfn_sprintf is printf into a heap-allocated string (NULL when out of
// memory): the way to word a fatal result or an error.
char *wasmfn_sprintf(const char *format, ...);

// wasmfn_test_host is the host a native build talks to: NULL (every request
// fails with "no host HTTP in this build") unless a test installs a function
// that takes the JSON request and returns the JSON response (heap allocated).
// The wasi build ignores it.
extern char *(*wasmfn_test_host)(const char *request_json);

#endif
