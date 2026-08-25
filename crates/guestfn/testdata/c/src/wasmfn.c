// The function-wasm ABI glue (see wasmfn.h). Memory discipline: a fresh wasm
// instance serves each request and the host drops it afterwards, so nothing
// here is ever freed except scratch the glue itself allocated.
#include "wasmfn.h"

#include <cJSON.h>
#include <pb_decode.h>
#include <pb_encode.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fn.h"
#include "run_function.pb.h"

enum { default_ttl_seconds = 60 };

char *(*wasmfn_test_host)(const char *request_json) = NULL;

// ─── the host imports ───────────────────────────────────────────────────────

#ifdef __wasi__
__attribute__((import_module("wasmfn"), import_name("log"))) void wasmfn_host_log(uint32_t level, uint32_t ptr, uint32_t len);
__attribute__((import_module("wasmfn"), import_name("http"))) uint64_t wasmfn_host_http(uint32_t ptr, uint32_t len);
#endif

// ─── ABI v1 exports ─────────────────────────────────────────────────────────

#ifdef __wasi__
// The host allocates its wasmfn.http answers through this export too,
// re-entrantly while wasmfn_run is on the stack; libc's malloc is fine with
// that.
__attribute__((export_name("wasmfn_alloc"))) uint32_t wasmfn_alloc(uint32_t size) {
	return (uint32_t)(uintptr_t)malloc(size ? size : 1);
}

__attribute__((export_name("wasmfn_run"))) uint64_t wasmfn_run(uint32_t ptr, uint32_t len) {
	size_t n = 0;
	uint8_t *out = wasmfn_handle((const uint8_t *)(uintptr_t)ptr, len, &n);
	return ((uint64_t)(uintptr_t)out << 32) | (uint64_t)(uint32_t)n;
}
#endif

// ─── the handle ─────────────────────────────────────────────────────────────

static void set_meta(fnv1_RunFunctionResponse *rsp, const char *tag) {
	rsp->has_meta = true;
	rsp->meta.tag = (char *)tag;
	rsp->meta.has_ttl = true;
	rsp->meta.ttl.seconds = default_ttl_seconds;
	rsp->meta.ttl.nanos = 0;
}

// fatal_response makes rsp a fresh response carrying one fatal result, without
// allocating: even out of memory the host gets a decodable reply.
static void fatal_response(fnv1_RunFunctionResponse *rsp, const char *tag, const char *msg) {
	static fnv1_Result result;
	fnv1_RunFunctionResponse fresh = fnv1_RunFunctionResponse_init_zero;
	*rsp = fresh;
	set_meta(rsp, tag);
	result.severity = fnv1_Severity_SEVERITY_FATAL;
	result.message = (char *)msg;
	result.has_target = true;
	result.target = fnv1_Target_TARGET_COMPOSITE;
	rsp->results = &result;
	rsp->results_count = 1;
}

static uint8_t *encode(const fnv1_RunFunctionResponse *rsp, size_t *out_len) {
	size_t size = 0;
	*out_len = 0;
	if (!pb_get_encoded_size(&size, fnv1_RunFunctionResponse_fields, rsp)) {
		return NULL;
	}
	uint8_t *buf = malloc(size ? size : 1);
	if (!buf) {
		return NULL;
	}
	pb_ostream_t os = pb_ostream_from_buffer(buf, size);
	if (!pb_encode(&os, fnv1_RunFunctionResponse_fields, rsp)) {
		return NULL;
	}
	*out_len = os.bytes_written;
	return buf;
}

uint8_t *wasmfn_handle(const uint8_t *in, size_t in_len, size_t *out_len) {
	fnv1_RunFunctionRequest req = fnv1_RunFunctionRequest_init_zero;
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	pb_istream_t is = pb_istream_from_buffer(in, in_len);
	if (!pb_decode(&is, fnv1_RunFunctionRequest_fields, &req)) {
		const char *msg = wasmfn_sprintf("cannot decode RunFunctionRequest: %s", PB_GET_ERROR(&is));
		fatal_response(&rsp, "", msg ? msg : "cannot decode RunFunctionRequest");
		return encode(&rsp, out_len);
	}
	const char *tag = req.has_meta && req.meta.tag ? req.meta.tag : "";
	set_meta(&rsp, tag);
	const char *err = run_function(&req, &rsp);
	if (err) {
		fatal_response(&rsp, tag, err);
	}
	return encode(&rsp, out_len);
}

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

// ─── wasmfn.log ─────────────────────────────────────────────────────────────

static void emit(uint32_t level, const char *payload) {
#ifdef __wasi__
	wasmfn_host_log(level, (uint32_t)(uintptr_t)payload, (uint32_t)strlen(payload));
#else
	(void)level;
	fprintf(stderr, "wasmfn log %s\n", payload);
#endif
}

// log_record sends {"msg": msg, "kv": [k, v, ...]} at level.
static void log_record(uint32_t level, const char *msg, va_list kv) {
	cJSON *record = cJSON_CreateObject();
	if (!record) {
		return;
	}
	cJSON_AddStringToObject(record, "msg", msg);
	cJSON *pairs = cJSON_AddArrayToObject(record, "kv");
	for (const char *key; pairs && (key = va_arg(kv, const char *)) != NULL;) {
		const char *value = va_arg(kv, const char *);
		cJSON_AddItemToArray(pairs, cJSON_CreateString(key));
		cJSON_AddItemToArray(pairs, cJSON_CreateString(value ? value : ""));
	}
	char *payload = cJSON_PrintUnformatted(record);
	cJSON_Delete(record);
	if (payload) {
		emit(level, payload);
		free(payload);
	}
}

void wasmfn_log_info(const char *msg, ...) {
	va_list kv;
	va_start(kv, msg);
	log_record(0, msg, kv);
	va_end(kv);
}

void wasmfn_log_debug(const char *msg, ...) {
	va_list kv;
	va_start(kv, msg);
	log_record(1, msg, kv);
	va_end(kv);
}

// ─── base64 (standard alphabet, padded: how the host renders body bytes) ────

static const char b64_alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static char *base64_encode(const uint8_t *in, size_t n) {
	char *out = malloc((n + 2) / 3 * 4 + 1);
	if (!out) {
		return NULL;
	}
	char *p = out;
	for (size_t i = 0; i < n; i += 3) {
		uint32_t v = (uint32_t)in[i] << 16;
		if (i + 1 < n) {
			v |= (uint32_t)in[i + 1] << 8;
		}
		if (i + 2 < n) {
			v |= in[i + 2];
		}
		*p++ = b64_alphabet[v >> 18 & 63];
		*p++ = b64_alphabet[v >> 12 & 63];
		*p++ = i + 1 < n ? b64_alphabet[v >> 6 & 63] : '=';
		*p++ = i + 2 < n ? b64_alphabet[v & 63] : '=';
	}
	*p = '\0';
	return out;
}

static int b64_index(char c) {
	const char *at = c ? strchr(b64_alphabet, c) : NULL;
	return at ? (int)(at - b64_alphabet) : -1;
}

// base64_decode returns the decoded bytes (NUL-terminated for convenience),
// or NULL when s is not base64.
static uint8_t *base64_decode(const char *s, size_t *n) {
	size_t len = strlen(s);
	uint8_t *out = malloc(len / 4 * 3 + 3);
	if (!out) {
		return NULL;
	}
	size_t written = 0;
	uint32_t acc = 0;
	int bits = 0;
	for (size_t i = 0; i < len; i++) {
		if (s[i] == '=') {
			break;
		}
		int v = b64_index(s[i]);
		if (v < 0) {
			free(out);
			return NULL;
		}
		acc = acc << 6 | (uint32_t)v;
		bits += 6;
		if (bits >= 8) {
			bits -= 8;
			out[written++] = (uint8_t)(acc >> bits & 0xff);
		}
	}
	out[written] = '\0';
	*n = written;
	return out;
}

// ─── wasmfn.http ────────────────────────────────────────────────────────────

// call_host hands the JSON request to the host and returns its JSON answer
// (*n bytes), which lives in a buffer the host obtained from wasmfn_alloc.
static char *call_host(const char *payload, size_t *n, char **err) {
#ifdef __wasi__
	uint64_t packed = wasmfn_host_http((uint32_t)(uintptr_t)payload, (uint32_t)strlen(payload));
	if (packed == 0) {
		fail(err, "the host returned no response");
		return NULL;
	}
	*n = (size_t)(uint32_t)packed;
	return (char *)(uintptr_t)(uint32_t)(packed >> 32);
#else
	if (!wasmfn_test_host) {
		fail(err, "no host HTTP in this build");
		return NULL;
	}
	char *answer = wasmfn_test_host(payload);
	if (!answer) {
		fail(err, "the test host returned no response");
		return NULL;
	}
	*n = strlen(answer);
	return answer;
#endif
}

// wire_request renders req as the host's JSON: method (omitted when GET), url,
// headers as {name: [values]} and the body base64.
static char *wire_request(const wasmfn_http_request *req) {
	cJSON *o = cJSON_CreateObject();
	if (!o) {
		return NULL;
	}
	if (req->method && *req->method) {
		cJSON_AddStringToObject(o, "method", req->method);
	}
	cJSON_AddStringToObject(o, "url", req->url ? req->url : "");
	if (req->headers && req->headers[0]) {
		cJSON *headers = cJSON_AddObjectToObject(o, "headers");
		for (size_t i = 0; headers && req->headers[i] && req->headers[i + 1]; i += 2) {
			cJSON *values = cJSON_GetObjectItemCaseSensitive(headers, req->headers[i]);
			if (!values) {
				values = cJSON_AddArrayToObject(headers, req->headers[i]);
			}
			if (values) {
				cJSON_AddItemToArray(values, cJSON_CreateString(req->headers[i + 1]));
			}
		}
	}
	if (req->body && req->body_len) {
		char *b64 = base64_encode(req->body, req->body_len);
		if (b64) {
			cJSON_AddStringToObject(o, "body", b64);
			free(b64);
		}
	}
	char *payload = cJSON_PrintUnformatted(o);
	cJSON_Delete(o);
	return payload;
}

bool wasmfn_http_send(const wasmfn_http_request *req, wasmfn_http_response *rsp, char **err) {
	char *payload = wire_request(req);
	if (!payload) {
		return fail(err, "cannot encode the request");
	}
	size_t n = 0;
	const char *raw = call_host(payload, &n, err);
	free(payload);
	if (!raw) {
		return false;
	}
	cJSON *answer = cJSON_ParseWithLength(raw, n);
	if (!answer) {
		return fail(err, "the host's HTTP response could not be decoded");
	}
	const cJSON *status = cJSON_GetObjectItemCaseSensitive(answer, "status");
	rsp->status = cJSON_IsNumber(status) ? status->valueint : 0;
	if (rsp->status == 0) {
		// The host did not perform the request: refused, over a budget, or failed.
		const cJSON *reason = cJSON_GetObjectItemCaseSensitive(answer, "error");
		fail(err, cJSON_IsString(reason) && reason->valuestring[0] ? reason->valuestring : "the host returned no status and no error");
		cJSON_Delete(answer);
		return false;
	}
	rsp->headers = cJSON_DetachItemFromObjectCaseSensitive(answer, "headers");
	if (!rsp->headers) {
		rsp->headers = cJSON_CreateObject();
	}
	const cJSON *body = cJSON_GetObjectItemCaseSensitive(answer, "body");
	rsp->body_len = 0;
	rsp->body = cJSON_IsString(body) ? base64_decode(body->valuestring, &rsp->body_len) : (uint8_t *)strdup("");
	cJSON_Delete(answer);
	if (!rsp->body) {
		return fail(err, "the host's HTTP response body is not base64");
	}
	return true;
}

const char *wasmfn_http_header(const wasmfn_http_response *rsp, const char *name) {
	const cJSON *values = rsp->headers ? cJSON_GetObjectItem(rsp->headers, name) : NULL;
	const cJSON *first = cJSON_IsArray(values) ? cJSON_GetArrayItem(values, 0) : NULL;
	return cJSON_IsString(first) ? first->valuestring : NULL;
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
