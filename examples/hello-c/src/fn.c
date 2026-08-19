// The hello-c guest: a Crossplane composition function in C, compiled to a
// wasip1 reactor by zig cc and run by function-wasm. It composes a ConfigMap
// greeting the composite resource.
//
// run_function is ordinary C over the structs nanopb generated from the
// vendored crossplane proto (src/fnv1); the ABI exports and the wasmfn.log /
// wasmfn.http host imports live in wasmfn.c. Nothing here is wasi-specific, so
// the logic also builds and tests natively (zig build test).
#include "fn.h"

#include <stdlib.h>
#include <string.h>

#include "structpb.h"
#include "wasmfn.h"

typedef enum { CONFIG_ABSENT, CONFIG_STRING, CONFIG_NOT_STRING } config_result;

// config_string reads a string field of the Input's config block.
static config_result config_string(const fnv1_RunFunctionRequest *req, const char *key, const char **out) {
	const google_protobuf_Value *v = req->has_input ? structpb_lookup(&req->input, "config", key, NULL) : NULL;
	if (!v) {
		return CONFIG_ABSENT;
	}
	const char *s = structpb_string(v);
	if (!s) {
		return CONFIG_NOT_STRING;
	}
	*out = s;
	return CONFIG_STRING;
}

static const char *observed_name(const fnv1_RunFunctionRequest *req) {
	if (!req->has_observed || !req->observed.has_composite || !req->observed.composite.has_resource) {
		return NULL;
	}
	return structpb_string(structpb_lookup(&req->observed.composite.resource, "metadata", "name", NULL));
}

// state_add_resource adds resource under key to the state's resources, in a
// new array: a state copied from the request is never modified in place.
static bool state_add_resource(fnv1_State *state, const char *key, google_protobuf_Struct resource) {
	size_t n = state->resources_count;
	fnv1_State_ResourcesEntry *entries = calloc(n + 1, sizeof *entries);
	if (!entries) {
		return false;
	}
	if (n) {
		memcpy(entries, state->resources, n * sizeof *entries);
	}
	entries[n].key = (char *)key;
	entries[n].has_value = true;
	entries[n].value.has_resource = true;
	entries[n].value.resource = resource;
	state->resources = entries;
	state->resources_count = (pb_size_t)(n + 1);
	return true;
}

const char *run_function(const fnv1_RunFunctionRequest *req, fnv1_RunFunctionResponse *rsp) {
	wasmfn_log_info("Running function", "tag", req->has_meta && req->meta.tag ? req->meta.tag : "", NULL);

	const char *greeting = "hello";
	if (config_string(req, "greeting", &greeting) == CONFIG_NOT_STRING) {
		return "cannot read config: greeting must be a string";
	}
	// greetingUrl fetches the greeting through the host instead - the
	// requires.egress grant of the module's manifest decides whether it may.
	const char *url = NULL;
	switch (config_string(req, "greetingUrl", &url)) {
	case CONFIG_NOT_STRING:
		return "cannot read config: greetingUrl must be a string";
	case CONFIG_STRING: {
		char *err = NULL;
		greeting = wasmfn_http_get_text(url, &err);
		if (!greeting) {
			const char *msg = wasmfn_sprintf("cannot fetch greeting: %s", err ? err : "unknown error");
			return msg ? msg : "cannot fetch greeting";
		}
		break;
	}
	case CONFIG_ABSENT:
		break;
	}

	const char *name = observed_name(req);
	if (!name) {
		return "cannot get observed composite resource: none in request";
	}

	// The ConfigMap greeting the composite, added to the desired state the
	// request carried.
	google_protobuf_Struct data = google_protobuf_Struct_init_zero;
	google_protobuf_Struct cm = google_protobuf_Struct_init_zero;
	char *text = wasmfn_sprintf("%s %s", greeting, name);
	if (!text || !structpb_set(&data, "greeting", structpb_string_value(text)) ||
	    !structpb_set(&cm, "apiVersion", structpb_string_value("v1")) ||
	    !structpb_set(&cm, "kind", structpb_string_value("ConfigMap")) ||
	    !structpb_set(&cm, "data", structpb_struct_value(data))) {
		return "out of memory";
	}
	if (req->has_desired) {
		rsp->desired = req->desired;
	}
	rsp->has_desired = true;
	if (!state_add_resource(&rsp->desired, "greeting", cm)) {
		return "out of memory";
	}

	fnv1_Result *result = calloc(1, sizeof *result);
	fnv1_Condition *condition = calloc(1, sizeof *condition);
	char *message = wasmfn_sprintf("greeted %s", name);
	if (!result || !condition || !message) {
		return "out of memory";
	}
	result->severity = fnv1_Severity_SEVERITY_NORMAL;
	result->message = message;
	result->has_target = true;
	result->target = fnv1_Target_TARGET_COMPOSITE;
	rsp->results = result;
	rsp->results_count = 1;
	condition->type = "FunctionSuccess";
	condition->status = fnv1_Status_STATUS_CONDITION_TRUE;
	condition->reason = "Success";
	condition->has_target = true;
	condition->target = fnv1_Target_TARGET_COMPOSITE_AND_CLAIM;
	rsp->conditions = condition;
	rsp->conditions_count = 1;
	return NULL;
}
