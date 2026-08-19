// Native unit tests of the function and the ABI glue: zig build test.
#include <pb_decode.h>
#include <pb_encode.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fn.h"
#include "structpb.h"
#include "wasmfn.h"

static int failures;

#define EXPECT(cond) expect(__LINE__, (cond), #cond)
#define EXPECT_STR(want, got) expect_str(__LINE__, (want), (got))

static void expect(int line, bool ok, const char *what) {
	if (!ok) {
		fprintf(stderr, "fn_test.c:%d: %s is false\n", line, what);
		failures++;
	}
}

static void expect_str(int line, const char *want, const char *got) {
	if (!got || strcmp(want, got) != 0) {
		fprintf(stderr, "fn_test.c:%d: want %s, got %s\n", line, want, got ? got : "(null)");
		failures++;
	}
}

// ─── fixtures ───────────────────────────────────────────────────────────────

static google_protobuf_Struct object(const char *key, google_protobuf_Value v) {
	google_protobuf_Struct s = google_protobuf_Struct_init_zero;
	if (!structpb_set(&s, key, v)) {
		abort();
	}
	return s;
}

// xr is an observed state whose composite resource is named name.
static fnv1_State xr(const char *name) {
	google_protobuf_Struct res = object("apiVersion", structpb_string_value("example.org/v1"));
	if (!structpb_set(&res, "kind", structpb_string_value("XR")) ||
	    !structpb_set(&res, "metadata", structpb_struct_value(object("name", structpb_string_value(name))))) {
		abort();
	}
	fnv1_State state = fnv1_State_init_zero;
	state.has_composite = true;
	state.composite.has_resource = true;
	state.composite.resource = res;
	return state;
}

static fnv1_RunFunctionRequest request(const char *tag, const char *name) {
	fnv1_RunFunctionRequest req = fnv1_RunFunctionRequest_init_zero;
	req.has_meta = true;
	req.meta.tag = (char *)tag;
	req.has_observed = true;
	req.observed = xr(name);
	return req;
}

// with_config sets the Input's config block to {key: v}.
static void with_config(fnv1_RunFunctionRequest *req, const char *key, google_protobuf_Value v) {
	req->has_input = true;
	req->input = object("config", structpb_struct_value(object(key, v)));
}

static fnv1_RunFunctionResponse response(void) {
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	return rsp;
}

// greeting_of reads data.greeting of the last desired resource.
static const char *greeting_of(const fnv1_RunFunctionResponse *rsp) {
	if (!rsp->has_desired || rsp->desired.resources_count == 0) {
		return NULL;
	}
	const fnv1_State_ResourcesEntry *last = &rsp->desired.resources[rsp->desired.resources_count - 1];
	return structpb_string(structpb_lookup(&last->value.resource, "data", "greeting", NULL));
}

// ─── tests ──────────────────────────────────────────────────────────────────

static void test_default_greeting(void) {
	fnv1_RunFunctionRequest req = request("hello", "my-xr");
	fnv1_RunFunctionResponse rsp = response();
	EXPECT(run_function(&req, &rsp) == NULL);
	EXPECT_STR("hello my-xr", greeting_of(&rsp));
	EXPECT(rsp.results_count == 1);
	EXPECT_STR("greeted my-xr", rsp.results[0].message);
	EXPECT(rsp.results[0].severity == fnv1_Severity_SEVERITY_NORMAL);
	EXPECT(rsp.conditions_count == 1);
	EXPECT_STR("FunctionSuccess", rsp.conditions[0].type);
}

static void test_configured_greeting_keeps_desired(void) {
	fnv1_RunFunctionRequest req = request("hello", "my-xr");
	with_config(&req, "greeting", structpb_string_value("hi"));
	fnv1_State_ResourcesEntry other = fnv1_State_ResourcesEntry_init_zero;
	other.key = "other";
	other.has_value = true;
	req.has_desired = true;
	req.desired.resources = &other;
	req.desired.resources_count = 1;
	fnv1_RunFunctionResponse rsp = response();
	EXPECT(run_function(&req, &rsp) == NULL);
	EXPECT_STR("hi my-xr", greeting_of(&rsp));
	EXPECT(rsp.desired.resources_count == 2);
	EXPECT_STR("other", rsp.desired.resources[0].key);
	EXPECT(req.desired.resources_count == 1);
}

static void test_bad_config_is_an_error(void) {
	fnv1_RunFunctionRequest req = request("hello", "my-xr");
	with_config(&req, "greeting", structpb_number_value(7));
	fnv1_RunFunctionResponse rsp = response();
	EXPECT_STR("cannot read config: greeting must be a string", run_function(&req, &rsp));
}

static char *fake_host(const char *request_json) {
	if (strstr(request_json, "\"url\":\"https://greetings.example.com/en\"")) {
		return strdup("{\"status\":200,\"headers\":{\"Content-Type\":[\"text/plain\"]},\"body\":\"aG93ZHkK\"}"); // "howdy\n"
	}
	return strdup("{\"status\":0,\"error\":\"sandbox.egress: no rule admits host \\\"evil.example.com\\\"\"}");
}

static void test_greeting_from_url_through_the_host(void) {
	wasmfn_test_host = fake_host;
	fnv1_RunFunctionRequest ok = request("hello", "my-xr");
	with_config(&ok, "greetingUrl", structpb_string_value("https://greetings.example.com/en"));
	fnv1_RunFunctionResponse rsp = response();
	EXPECT(run_function(&ok, &rsp) == NULL);
	EXPECT_STR("howdy my-xr", greeting_of(&rsp));

	fnv1_RunFunctionRequest bad = request("hello", "my-xr");
	with_config(&bad, "greetingUrl", structpb_string_value("https://evil.example.com/en"));
	rsp = response();
	EXPECT_STR("cannot fetch greeting: sandbox.egress: no rule admits host \"evil.example.com\"", run_function(&bad, &rsp));

	wasmfn_http_response full = {0};
	wasmfn_http_request get = {.url = "https://greetings.example.com/en"};
	char *err = NULL;
	EXPECT(wasmfn_http_send(&get, &full, &err));
	EXPECT(full.status == 200);
	EXPECT_STR("text/plain", wasmfn_http_header(&full, "content-type"));
	EXPECT_STR("howdy\n", (const char *)full.body);
	wasmfn_test_host = NULL;

	rsp = response();
	EXPECT_STR("cannot fetch greeting: no host HTTP in this build", run_function(&ok, &rsp));
}

// The glue: a request on the wire comes back as a response on the wire.
static void test_handle_round_trip(void) {
	fnv1_RunFunctionRequest req = request("hello", "my-xr");
	size_t size = 0;
	EXPECT(pb_get_encoded_size(&size, fnv1_RunFunctionRequest_fields, &req));
	uint8_t *in = malloc(size);
	pb_ostream_t os = pb_ostream_from_buffer(in, size);
	EXPECT(pb_encode(&os, fnv1_RunFunctionRequest_fields, &req));

	size_t n = 0;
	uint8_t *out = wasmfn_handle(in, os.bytes_written, &n);
	EXPECT(out != NULL && n > 0);
	fnv1_RunFunctionResponse rsp = response();
	pb_istream_t is = pb_istream_from_buffer(out, n);
	EXPECT(pb_decode(&is, fnv1_RunFunctionResponse_fields, &rsp));
	EXPECT_STR("hello", rsp.meta.tag);
	EXPECT(rsp.meta.has_ttl && rsp.meta.ttl.seconds == 60);
	EXPECT_STR("hello my-xr", greeting_of(&rsp));

	// Garbage in, a fatal result out.
	const uint8_t junk[] = {0xff, 0xff, 0xff, 0xff};
	out = wasmfn_handle(junk, sizeof junk, &n);
	EXPECT(out != NULL && n > 0);
	fnv1_RunFunctionResponse fatal = response();
	is = pb_istream_from_buffer(out, n);
	EXPECT(pb_decode(&is, fnv1_RunFunctionResponse_fields, &fatal));
	EXPECT(fatal.results_count == 1 && fatal.results[0].severity == fnv1_Severity_SEVERITY_FATAL);
	EXPECT(fatal.results[0].message && strncmp(fatal.results[0].message, "cannot decode RunFunctionRequest", 32) == 0);
}

int main(void) {
	test_default_greeting();
	test_configured_greeting_keeps_desired();
	test_bad_config_is_an_error();
	test_greeting_from_url_through_the_host();
	test_handle_round_trip();
	if (failures) {
		fprintf(stderr, "%d check(s) failed\n", failures);
		return 1;
	}
	return 0;
}
