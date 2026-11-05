// The cert-rotator guest: a Crossplane composition function whose logic is
// C++ (rotator.cpp over tls.cpp and mbedTLS), run by function-wasm as an
// ABI v2 component. It keeps a TLS Secret's self-signed certificate fresh:
// it asks Crossplane for the Secret the composite resource names, through
// the response's requirements, reads the certificate's validity, and
// composes the Secret again - carried forward while the certificate has
// more than renewBeforeDays left, with a freshly issued certificate and key
// otherwise.
//
// This file is the C adapter between the generated protobuf structs (nanopb
// over the vendored crossplane proto, src/fnv1: a ResourceSelector carries a
// field named `namespace`, which no C++ translation unit can include) and
// rotator.h, the decision's C interface. The world's run export and the log
// import live in wasmfn.c, behind the bindings wit-bindgen generated from
// wit/ (src/gen). Nothing here is wasi-specific, so the logic also builds
// and tests natively (zig build test).
#include "fn.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "rotator.h"
#include "structpb.h"
#include "wasmfn.h"

// The composition resource name of the Secret, and the key the ask travels
// under in both directions.
static const char *const resource_key = "certificate";

enum { default_validity_days = 90, default_renew_before_days = 30 };

static const google_protobuf_Struct *observed_composite(const fnv1_RunFunctionRequest *req) {
	if (!req->has_observed || !req->observed.has_composite || !req->observed.composite.has_resource) {
		return NULL;
	}
	return &req->observed.composite.resource;
}

// number_of reads a number value as an int, rounding, or leaves *out alone.
static bool number_of(const google_protobuf_Value *v, int *out) {
	if (!v || v->which_kind != google_protobuf_Value_number_value_tag) {
		return false;
	}
	*out = (int)lround(v->kind.number_value);
	return true;
}

// strings_of reads a list of strings into a NULL-terminated array; a list
// with a non-string element is NULL.
static const char *const *strings_of(const google_protobuf_Value *v, size_t *count) {
	*count = 0;
	if (!v) {
		return NULL;
	}
	if (v->which_kind != google_protobuf_Value_list_value_tag) {
		return NULL;
	}
	size_t n = v->kind.list_value.values_count;
	const char **out = calloc(n + 1, sizeof *out);
	if (!out) {
		return NULL;
	}
	for (size_t i = 0; i < n; i++) {
		out[i] = structpb_string(&v->kind.list_value.values[i]);
		if (!out[i]) {
			return NULL;
		}
	}
	*count = n;
	return out;
}

// state_set_resource puts resource under key in the state's resources,
// replacing an entry of that key, in a new array: a state copied from the
// request is never modified in place.
static bool state_set_resource(fnv1_State *state, const char *key, google_protobuf_Struct resource) {
	size_t n = state->resources_count;
	fnv1_State_ResourcesEntry *entries = calloc(n + 1, sizeof *entries);
	if (!entries) {
		return false;
	}
	size_t at = n;
	for (size_t i = 0; i < n; i++) {
		entries[i] = state->resources[i];
		if (entries[i].key && strcmp(entries[i].key, key) == 0) {
			at = i;
		}
	}
	entries[at].key = (char *)key;
	entries[at].has_value = true;
	entries[at].value.has_resource = true;
	entries[at].value.resource = resource;
	state->resources = entries;
	state->resources_count = (pb_size_t)(at == n ? n + 1 : n);
	return true;
}

// require_secret asks Crossplane for the Secret, by name in the composite
// resource's namespace - on every call, since Crossplane calls again with
// the answer only while the ask is repeated.
static bool require_secret(fnv1_RunFunctionResponse *rsp, const char *name, const char *ns) {
	fnv1_Requirements_ResourcesEntry *entry = calloc(1, sizeof *entry);
	if (!entry) {
		return false;
	}
	entry->key = (char *)resource_key;
	entry->has_value = true;
	entry->value.api_version = "v1";
	entry->value.kind = "Secret";
	entry->value.which_match = fnv1_ResourceSelector_match_name_tag;
	entry->value.match.match_name = (char *)name;
	entry->value.namespace = (char *)ns;
	rsp->has_requirements = true;
	rsp->requirements.resources = entry;
	rsp->requirements.resources_count = 1;
	return true;
}

// answer is what Crossplane fetched for the ask, or NULL on the first call,
// before it did. The key alone is the answer: a Secret that does not exist
// is an empty Resources message, which an encoder may leave off the wire.
static const fnv1_Resources *answer(const fnv1_RunFunctionRequest *req) {
	static const fnv1_Resources none = fnv1_Resources_init_zero;
	for (size_t i = 0; i < req->required_resources_count; i++) {
		const fnv1_RunFunctionRequest_RequiredResourcesEntry *e = &req->required_resources[i];
		if (e->key && strcmp(e->key, resource_key) == 0) {
			return e->has_value ? &e->value : &none;
		}
	}
	return NULL;
}

static google_protobuf_Struct secret_of(const char *name, const rotator_result *out, bool *ok) {
	google_protobuf_Struct data = google_protobuf_Struct_init_zero;
	google_protobuf_Struct metadata = google_protobuf_Struct_init_zero;
	google_protobuf_Struct secret = google_protobuf_Struct_init_zero;
	*ok = structpb_set(&data, "tls.crt", structpb_string_value(out->certificate_b64)) &&
	      structpb_set(&data, "tls.key", structpb_string_value(out->key_b64)) &&
	      structpb_set(&metadata, "name", structpb_string_value(name)) &&
	      structpb_set(&secret, "apiVersion", structpb_string_value("v1")) &&
	      structpb_set(&secret, "kind", structpb_string_value("Secret")) &&
	      structpb_set(&secret, "type", structpb_string_value("kubernetes.io/tls")) &&
	      structpb_set(&secret, "metadata", structpb_struct_value(metadata)) &&
	      structpb_set(&secret, "data", structpb_struct_value(data));
	return secret;
}

// set_status writes status.certificate onto the desired composite resource,
// keeping whatever status the earlier steps desired.
static bool set_status(fnv1_RunFunctionResponse *rsp, const char *secret_name, const rotator_result *out) {
	google_protobuf_Struct certificate = google_protobuf_Struct_init_zero;
	if (!structpb_set(&certificate, "secretName", structpb_string_value(secret_name)) ||
	    !structpb_set(&certificate, "subject", structpb_string_value(out->subject)) ||
	    !structpb_set(&certificate, "serialNumber", structpb_string_value(out->serial)) ||
	    !structpb_set(&certificate, "notBefore", structpb_string_value(out->not_before)) ||
	    !structpb_set(&certificate, "notAfter", structpb_string_value(out->not_after)) ||
	    !structpb_set(&certificate, "daysLeft", structpb_number_value((double)(out->seconds_left / 86400))) ||
	    !structpb_set(&certificate, "rotated", structpb_bool_value(out->rotated)) ||
	    !structpb_set(&certificate, "reason", structpb_string_value(out->reason))) {
		return false;
	}
	google_protobuf_Struct resource = google_protobuf_Struct_init_zero;
	google_protobuf_Struct status = google_protobuf_Struct_init_zero;
	if (rsp->desired.has_composite && rsp->desired.composite.has_resource) {
		resource = rsp->desired.composite.resource;
		const google_protobuf_Struct *existing = structpb_struct(structpb_get(&resource, "status"));
		if (existing) {
			status = *existing;
		}
	}
	if (!structpb_set(&status, "certificate", structpb_struct_value(certificate)) ||
	    !structpb_set(&resource, "status", structpb_struct_value(status))) {
		return false;
	}
	rsp->desired.has_composite = true;
	rsp->desired.composite.has_resource = true;
	rsp->desired.composite.resource = resource;
	return true;
}

const char *run_function(const fnv1_RunFunctionRequest *req, fnv1_RunFunctionResponse *rsp) {
	wasmfn_log_info("Running function", "tag", req->has_meta && req->meta.tag ? req->meta.tag : "", NULL);

	const google_protobuf_Struct *xr = observed_composite(req);
	if (!xr) {
		return "cannot get observed composite resource: none in request";
	}
	const char *name = structpb_string(structpb_lookup(xr, "metadata", "name", NULL));
	const char *ns = structpb_string(structpb_lookup(xr, "metadata", "namespace", NULL));
	if (!name) {
		return "cannot get observed composite resource: it has no metadata.name";
	}
	if (!ns) {
		return "the composite resource has no namespace: a Secret is namespaced, so the composite resource that owns it must be";
	}
	const char *secret_name = structpb_string(structpb_lookup(xr, "spec", "secretName", NULL));
	if (!secret_name) {
		secret_name = wasmfn_sprintf("%s-tls", name);
		if (!secret_name) {
			return "out of memory";
		}
	}
	const char *common_name = structpb_string(structpb_lookup(xr, "spec", "commonName", NULL));
	if (!common_name) {
		common_name = name;
	}
	const google_protobuf_Value *names = structpb_lookup(xr, "spec", "dnsNames", NULL);
	size_t dns_count = 0;
	const char *const *dns_names = strings_of(names, &dns_count);
	if (names && !dns_names) {
		return "cannot read spec.dnsNames: it must be a list of strings";
	}
	int validity_days = default_validity_days;
	int renew_before_days = default_renew_before_days;
	number_of(structpb_lookup(xr, "spec", "validityDays", NULL), &validity_days);
	number_of(structpb_lookup(xr, "spec", "renewBeforeDays", NULL), &renew_before_days);
	if (validity_days < 1) {
		return "spec.validityDays must be at least 1";
	}
	if (renew_before_days < 0 || renew_before_days >= validity_days) {
		return "spec.renewBeforeDays must be at least 0 and less than spec.validityDays, or every run would rotate";
	}

	// The desired state the request carried, carried forward.
	if (req->has_desired) {
		rsp->desired = req->desired;
	}
	rsp->has_desired = true;
	if (!require_secret(rsp, secret_name, ns)) {
		return "out of memory";
	}
	const fnv1_Resources *secret = answer(req);
	if (!secret) {
		// The first call of the reconcile: Crossplane fetches the Secret and
		// calls again with it.
		wasmfn_log_debug("Asking Crossplane for the Secret", "name", secret_name, "namespace", ns, NULL);
		return NULL;
	}

	rotator_request ask = {
	    .common_name = common_name,
	    .dns_names = dns_names,
	    .dns_names_count = dns_count,
	    .validity_days = validity_days,
	    .renew_before_days = renew_before_days,
	    .secret_found = false,
	    .certificate_b64 = NULL,
	    .key_b64 = NULL,
	    .now = (int64_t)time(NULL),
	};
	if (secret->items_count > 0 && secret->items[0].has_resource) {
		const google_protobuf_Struct *found = &secret->items[0].resource;
		ask.secret_found = true;
		ask.certificate_b64 = structpb_string(structpb_lookup(found, "data", "tls.crt", NULL));
		ask.key_b64 = structpb_string(structpb_lookup(found, "data", "tls.key", NULL));
	}
	rotator_result out;
	const char *err = rotator_decide(&ask, &out);
	if (err) {
		return err;
	}
	wasmfn_log_info(out.rotated ? "Rotating the certificate" : "Keeping the certificate", "secret", secret_name, "reason", out.reason, "notAfter", out.not_after, NULL);

	bool ok = false;
	google_protobuf_Struct composed = secret_of(secret_name, &out, &ok);
	if (!ok || !state_set_resource(&rsp->desired, resource_key, composed) || !set_status(rsp, secret_name, &out)) {
		return "out of memory";
	}

	fnv1_Result *result = calloc(1, sizeof *result);
	fnv1_Condition *condition = calloc(1, sizeof *condition);
	char *message = out.rotated ? wasmfn_sprintf("rotated the certificate in Secret %s (%s): valid until %s", secret_name, out.reason, out.not_after)
	                            : wasmfn_sprintf("the certificate in Secret %s is valid until %s", secret_name, out.not_after);
	if (!result || !condition || !message) {
		return "out of memory";
	}
	result->severity = fnv1_Severity_SEVERITY_NORMAL;
	result->message = message;
	result->has_target = true;
	result->target = fnv1_Target_TARGET_COMPOSITE;
	rsp->results = result;
	rsp->results_count = 1;
	condition->type = "CertificateReady";
	condition->status = fnv1_Status_STATUS_CONDITION_TRUE;
	condition->reason = out.rotated ? "Rotated" : "Valid";
	condition->has_target = true;
	condition->target = fnv1_Target_TARGET_COMPOSITE_AND_CLAIM;
	rsp->conditions = condition;
	rsp->conditions_count = 1;
	return NULL;
}
