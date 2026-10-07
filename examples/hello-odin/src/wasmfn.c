// The wasi:http@0.2 client of the c flavour's glue (see wasmfn.h), unchanged
// but for what was cut: the run export, the nanopb handle and the log import,
// which src/wasmfn.odin implements. Memory discipline: a fresh component
// instance serves each request and the host drops it afterwards, so nothing
// here is ever freed except scratch the client itself allocated.
#include "wasmfn.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

// The wit-bindgen c bindings of this guest's world (src/gen): the wasi:http
// client interfaces.
#include "function.h"

enum { read_chunk = 64 * 1024, write_chunk = 4096 };

// ─── strings ────────────────────────────────────────────────────────────────

char *wasmfn_sprintf(const char *format, ...) {
	va_list ap, copy;
	va_start(ap, format);
	va_copy(copy, ap);
	int n = vsnprintf(NULL, 0, format, ap);
	va_end(ap);
	char *s = n < 0 ? NULL : malloc((size_t)n + 1);
	if (s) {
		vsnprintf(s, (size_t)n + 1, format, copy);
	}
	va_end(copy);
	return s;
}

static bool is_space(char c) {
	return c == ' ' || c == '\t' || c == '\r' || c == '\n';
}

static bool fail(char **err, const char *msg) {
	if (err) {
		*err = strdup(msg);
	}
	return false;
}

// ─── HTTP over wasi:http@0.2 ────────────────────────────────────────────────

// The WIT names of wasi:http's error-code cases, in tag order.
static const char *const error_code_names[] = {
    "dns-timeout",
    "dns-error",
    "destination-not-found",
    "destination-unavailable",
    "destination-ip-prohibited",
    "destination-ip-unroutable",
    "connection-refused",
    "connection-terminated",
    "connection-timeout",
    "connection-read-timeout",
    "connection-write-timeout",
    "connection-limit-reached",
    "tls-protocol-error",
    "tls-certificate-error",
    "tls-alert-received",
    "http-request-denied",
    "http-request-length-required",
    "http-request-body-size",
    "http-request-method-invalid",
    "http-request-uri-invalid",
    "http-request-uri-too-long",
    "http-request-header-section-size",
    "http-request-header-size",
    "http-request-trailer-section-size",
    "http-request-trailer-size",
    "http-response-incomplete",
    "http-response-header-section-size",
    "http-response-header-size",
    "http-response-body-size",
    "http-response-trailer-section-size",
    "http-response-trailer-size",
    "http-response-transfer-coding",
    "http-response-content-coding",
    "http-response-timeout",
    "http-upgrade-failed",
    "http-protocol-error",
    "loop-detected",
    "configuration-error",
    "internal-error",
};

// fail_with hands an allocated message to *err (or frees it when nobody
// listens).
static bool fail_with(char **err, char *msg) {
	if (err) {
		*err = msg ? msg : strdup("out of memory");
	} else {
		free(msg);
	}
	return false;
}

// describe_error_code words a wasi:http error-code. internal-error carries
// the host's own reason - the refusal of the egress grant or policy, a
// budget, a transport failure - verbatim; every other case is named.
static char *describe_error_code(const wasi_http_types_error_code_t *code) {
	if (code->tag == WASI_HTTP_TYPES_ERROR_CODE_INTERNAL_ERROR && code->val.internal_error.is_some) {
		const function_string_t *reason = &code->val.internal_error.val;
		return strndup((const char *)reason->ptr, reason->len);
	}
	size_t known = sizeof error_code_names / sizeof *error_code_names;
	return wasmfn_sprintf("wasi:http error-code %s", code->tag < known ? error_code_names[code->tag] : "unknown");
}

static bool split_url(const char *url, wasi_http_types_scheme_t *scheme, function_string_t *authority, function_string_t *path, char **err) {
	const char *rest;
	if (url && strncmp(url, "https://", 8) == 0) {
		scheme->tag = WASI_HTTP_TYPES_SCHEME_HTTPS;
		rest = url + 8;
	} else if (url && strncmp(url, "http://", 7) == 0) {
		scheme->tag = WASI_HTTP_TYPES_SCHEME_HTTP;
		rest = url + 7;
	} else {
		return fail(err, "only http and https URLs work");
	}
	const char *slash = strchr(rest, '/');
	size_t authority_len = slash ? (size_t)(slash - rest) : strlen(rest);
	if (!authority_len) {
		return fail(err, "the URL has no host");
	}
	function_string_dup_n(authority, rest, authority_len);
	function_string_dup(path, slash ? slash : "/");
	return true;
}

static void method_of(const char *name, wasi_http_types_method_t *method) {
	static const struct {
		const char *name;
		uint8_t tag;
	} standard[] = {
	    {"GET", WASI_HTTP_TYPES_METHOD_GET},         {"HEAD", WASI_HTTP_TYPES_METHOD_HEAD},
	    {"POST", WASI_HTTP_TYPES_METHOD_POST},       {"PUT", WASI_HTTP_TYPES_METHOD_PUT},
	    {"DELETE", WASI_HTTP_TYPES_METHOD_DELETE},   {"CONNECT", WASI_HTTP_TYPES_METHOD_CONNECT},
	    {"OPTIONS", WASI_HTTP_TYPES_METHOD_OPTIONS}, {"TRACE", WASI_HTTP_TYPES_METHOD_TRACE},
	    {"PATCH", WASI_HTTP_TYPES_METHOD_PATCH},
	};
	if (!name || !*name) {
		method->tag = WASI_HTTP_TYPES_METHOD_GET;
		return;
	}
	for (size_t i = 0; i < sizeof standard / sizeof *standard; i++) {
		if (strcmp(standard[i].name, name) == 0) {
			method->tag = standard[i].tag;
			return;
		}
	}
	method->tag = WASI_HTTP_TYPES_METHOD_OTHER;
	function_string_dup(&method->val.other, name);
}

// request_headers builds the fields resource of the request's headers.
static bool request_headers(const wasmfn_http_request *req, wasi_http_types_own_fields_t *headers, char **err) {
	*headers = wasi_http_types_constructor_fields();
	for (size_t i = 0; req->headers && req->headers[i] && req->headers[i + 1]; i += 2) {
		wasi_http_types_field_name_t name;
		function_string_set(&name, req->headers[i]);
		wasi_http_types_field_value_t value = {(uint8_t *)req->headers[i + 1], strlen(req->headers[i + 1])};
		wasi_http_types_header_error_t herr;
		if (!wasi_http_types_method_fields_append(wasi_http_types_borrow_fields(*headers), &name, &value, &herr)) {
			return fail_with(err, wasmfn_sprintf("the host refused the request header %s", req->headers[i]));
		}
	}
	return true;
}

// write_body streams the request body and finishes it; the request has
// already been handed to the handler, which reads as we write.
static bool write_body(wasi_http_types_own_outgoing_body_t body, const uint8_t *bytes, size_t len, char **err) {
	wasi_http_types_own_output_stream_t stream;
	if (!wasi_http_types_method_outgoing_body_write(wasi_http_types_borrow_outgoing_body(body), &stream)) {
		return fail(err, "the request body stream was already taken");
	}
	for (size_t off = 0; off < len; off += write_chunk) {
		size_t n = len - off < write_chunk ? len - off : write_chunk;
		function_list_u8_t chunk = {(uint8_t *)bytes + off, n};
		wasi_io_streams_stream_error_t serr;
		if (!wasi_io_streams_method_output_stream_blocking_write_and_flush(wasi_io_streams_borrow_output_stream(stream), &chunk, &serr)) {
			wasi_io_streams_output_stream_drop_own(stream);
			return fail(err, "writing the request body failed");
		}
	}
	wasi_io_streams_output_stream_drop_own(stream);
	wasi_http_types_error_code_t code;
	if (!wasi_http_types_static_outgoing_body_finish(body, NULL, &code)) {
		return fail_with(err, describe_error_code(&code));
	}
	return true;
}

// response_headers copies the response's headers out as alternating
// name/value C strings (values are bytes on the wire; a NUL inside one ends
// it here).
static bool response_headers(wasi_http_types_borrow_incoming_response_t response, const char ***out) {
	wasi_http_types_own_headers_t headers = wasi_http_types_method_incoming_response_headers(response);
	wasi_http_types_list_tuple2_field_name_field_value_t entries;
	wasi_http_types_method_fields_entries(wasi_http_types_borrow_fields(headers), &entries);
	const char **pairs = calloc(2 * entries.len + 1, sizeof *pairs);
	if (pairs) {
		for (size_t i = 0; i < entries.len; i++) {
			pairs[2 * i] = strndup((const char *)entries.ptr[i].f0.ptr, entries.ptr[i].f0.len);
			pairs[2 * i + 1] = strndup((const char *)entries.ptr[i].f1.ptr, entries.ptr[i].f1.len);
		}
	}
	wasi_http_types_list_tuple2_field_name_field_value_free(&entries);
	wasi_http_types_fields_drop_own(headers);
	*out = pairs;
	return pairs != NULL;
}

// read_body drains the response body stream into one NUL-terminated buffer.
static bool read_body(wasi_http_types_borrow_incoming_response_t response, uint8_t **out, size_t *out_len, char **err) {
	wasi_http_types_own_incoming_body_t body;
	if (!wasi_http_types_method_incoming_response_consume(response, &body)) {
		return fail(err, "the response body was already taken");
	}
	wasi_http_types_own_input_stream_t stream;
	if (!wasi_http_types_method_incoming_body_stream(wasi_http_types_borrow_incoming_body(body), &stream)) {
		wasi_http_types_incoming_body_drop_own(body);
		return fail(err, "the response body stream was already taken");
	}
	uint8_t *buf = malloc(1);
	size_t len = 0;
	bool ok = buf != NULL;
	while (ok) {
		function_list_u8_t chunk;
		wasi_io_streams_stream_error_t serr;
		if (!wasi_io_streams_method_input_stream_blocking_read(wasi_io_streams_borrow_input_stream(stream), read_chunk, &chunk, &serr)) {
			ok = serr.tag == WASI_IO_STREAMS_STREAM_ERROR_CLOSED || fail(err, "reading the response body failed");
			break;
		}
		uint8_t *grown = realloc(buf, len + chunk.len + 1);
		if (!grown) {
			ok = fail(err, "out of memory");
			function_list_u8_free(&chunk);
			break;
		}
		buf = grown;
		memcpy(buf + len, chunk.ptr, chunk.len);
		len += chunk.len;
		function_list_u8_free(&chunk);
	}
	wasi_io_streams_input_stream_drop_own(stream);
	wasi_http_types_future_trailers_drop_own(wasi_http_types_static_incoming_body_finish(body));
	if (!ok) {
		free(buf);
		return false;
	}
	buf[len] = '\0';
	*out = buf;
	*out_len = len;
	return true;
}

// send_through_host performs req over wasi:http/outgoing-handler: build the
// request, hand it to the handler, stream the body, block on the response
// future, then read status, headers and body.
static bool send_through_host(const wasmfn_http_request *req, wasmfn_http_response *rsp, char **err) {
	wasi_http_types_scheme_t scheme;
	function_string_t authority, path;
	if (!split_url(req->url, &scheme, &authority, &path, err)) {
		return false;
	}
	wasi_http_types_own_fields_t headers;
	if (!request_headers(req, &headers, err)) {
		return false;
	}
	wasi_http_types_own_outgoing_request_t request = wasi_http_types_constructor_outgoing_request(headers);
	wasi_http_types_borrow_outgoing_request_t r = wasi_http_types_borrow_outgoing_request(request);
	wasi_http_types_method_t method;
	method_of(req->method, &method);
	if (!wasi_http_types_method_outgoing_request_set_method(r, &method)) {
		return fail(err, "the host refused the request method");
	}
	if (!wasi_http_types_method_outgoing_request_set_scheme(r, &scheme) ||
	    !wasi_http_types_method_outgoing_request_set_authority(r, &authority) ||
	    !wasi_http_types_method_outgoing_request_set_path_with_query(r, &path)) {
		return fail(err, "the host refused the URL");
	}
	bool has_body = req->body && req->body_len;
	wasi_http_types_own_outgoing_body_t body;
	if (has_body && !wasi_http_types_method_outgoing_request_body(r, &body)) {
		return fail(err, "the request body was already taken");
	}

	wasi_http_outgoing_handler_own_future_incoming_response_t future;
	wasi_http_outgoing_handler_error_code_t code;
	if (!wasi_http_outgoing_handler_handle(request, NULL, &future, &code)) {
		return fail_with(err, describe_error_code(&code));
	}
	if (has_body && !write_body(body, req->body, req->body_len, err)) {
		return false;
	}

	wasi_http_types_borrow_future_incoming_response_t f = wasi_http_types_borrow_future_incoming_response(future);
	wasi_io_poll_own_pollable_t ready = wasi_http_types_method_future_incoming_response_subscribe(f);
	wasi_io_poll_method_pollable_block(wasi_io_poll_borrow_pollable(ready));
	wasi_io_poll_pollable_drop_own(ready);
	wasi_http_types_result_result_own_incoming_response_error_code_void_t got;
	if (!wasi_http_types_method_future_incoming_response_get(f, &got)) {
		return fail(err, "the host answered nothing");
	}
	wasi_http_types_future_incoming_response_drop_own(future);
	if (got.is_err) {
		return fail(err, "the response was already taken");
	}
	if (got.val.ok.is_err) {
		return fail_with(err, describe_error_code(&got.val.ok.val.err));
	}
	wasi_http_types_own_incoming_response_t response = got.val.ok.val.ok;
	wasi_http_types_borrow_incoming_response_t ir = wasi_http_types_borrow_incoming_response(response);
	rsp->status = wasi_http_types_method_incoming_response_status(ir);
	if (!response_headers(ir, &rsp->headers)) {
		return fail(err, "out of memory");
	}
	bool ok = read_body(ir, &rsp->body, &rsp->body_len, err);
	wasi_http_types_incoming_response_drop_own(response);
	return ok;
}

bool wasmfn_http_send(const wasmfn_http_request *req, wasmfn_http_response *rsp, char **err) {
	return send_through_host(req, rsp, err);
}

const char *wasmfn_http_header(const wasmfn_http_response *rsp, const char *name) {
	for (size_t i = 0; rsp->headers && rsp->headers[i] && rsp->headers[i + 1]; i += 2) {
		if (strcasecmp(rsp->headers[i], name) == 0) {
			return rsp->headers[i + 1];
		}
	}
	return NULL;
}

char *wasmfn_http_get_text(const char *url, char **err) {
	wasmfn_http_request req = {.url = url};
	wasmfn_http_response rsp = {0};
	if (!wasmfn_http_send(&req, &rsp, err)) {
		return NULL;
	}
	if (rsp.status != 200) {
		char *msg = wasmfn_sprintf("GET %s: status %d", url, rsp.status);
		if (err) {
			*err = msg ? msg : strdup("unexpected status");
		}
		return NULL;
	}
	char *text = (char *)rsp.body;
	char *end = text + rsp.body_len;
	while (text < end && is_space(*text)) {
		text++;
	}
	while (end > text && is_space(end[-1])) {
		end--;
	}
	*end = '\0';
	return text;
}
