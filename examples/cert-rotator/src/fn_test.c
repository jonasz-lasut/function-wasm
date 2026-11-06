// Native unit tests of the function over the generated structs, end to end
// through run_function: zig build test. The crypto has its own tests in
// rotator_test.cpp; here the certificate is whatever the rotator issues.
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

static google_protobuf_Struct empty(void) {
	google_protobuf_Struct s = google_protobuf_Struct_init_zero;
	return s;
}

static google_protobuf_Struct object(const char *key, google_protobuf_Value v) {
	google_protobuf_Struct s = google_protobuf_Struct_init_zero;
	if (!structpb_set(&s, key, v)) {
		abort();
	}
	return s;
}

// xr is a request whose observed composite is a ServerCertificate named
// shop in team-a with spec.
static fnv1_RunFunctionRequest xr(google_protobuf_Struct spec) {
	google_protobuf_Struct metadata = object("name", structpb_string_value("shop"));
	google_protobuf_Struct res = object("apiVersion", structpb_string_value("example.org/v1alpha1"));
	if (!structpb_set(&metadata, "namespace", structpb_string_value("team-a")) ||
	    !structpb_set(&res, "kind", structpb_string_value("ServerCertificate")) ||
	    !structpb_set(&res, "metadata", structpb_struct_value(metadata)) ||
	    !structpb_set(&res, "spec", structpb_struct_value(spec))) {
		abort();
	}
	fnv1_RunFunctionRequest req = fnv1_RunFunctionRequest_init_zero;
	req.has_meta = true;
	req.meta.tag = "t";
	req.has_observed = true;
	req.observed.has_composite = true;
	req.observed.composite.has_resource = true;
	req.observed.composite.resource = res;
	return req;
}

// answered adds Crossplane's answer to the ask: the Secret, or none.
static void answered(fnv1_RunFunctionRequest *req, const google_protobuf_Struct *secret) {
	fnv1_RunFunctionRequest_RequiredResourcesEntry *entry = calloc(1, sizeof *entry);
	entry->key = "certificate";
	entry->has_value = true;
	if (secret) {
		fnv1_Resource *item = calloc(1, sizeof *item);
		item->has_resource = true;
		item->resource = *secret;
		entry->value.items = item;
		entry->value.items_count = 1;
	}
	req->required_resources = entry;
	req->required_resources_count = 1;
}

static const google_protobuf_Struct *composed_secret(const fnv1_RunFunctionResponse *rsp) {
	if (!rsp->has_desired) {
		return NULL;
	}
	for (size_t i = 0; i < rsp->desired.resources_count; i++) {
		if (strcmp(rsp->desired.resources[i].key, "certificate") == 0 && rsp->desired.resources[i].value.has_resource) {
			return &rsp->desired.resources[i].value.resource;
		}
	}
	return NULL;
}

static const google_protobuf_Struct *status_certificate(const fnv1_RunFunctionResponse *rsp) {
	if (!rsp->has_desired || !rsp->desired.has_composite || !rsp->desired.composite.has_resource) {
		return NULL;
	}
	return structpb_struct(structpb_lookup(&rsp->desired.composite.resource, "status", "certificate", NULL));
}

// "-----BEGIN CERTIFICATE-----" in base64: the prefix of every tls.crt.
static const char *const certificate_prefix = "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0t";

// ─── tests ──────────────────────────────────────────────────────────────────

static void test_no_composite(void) {
	fnv1_RunFunctionRequest req = fnv1_RunFunctionRequest_init_zero;
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	EXPECT_STR("cannot get observed composite resource: none in request", run_function(&req, &rsp));
}

static void test_first_call_asks_for_the_secret(void) {
	fnv1_RunFunctionRequest req = xr(object("secretName", structpb_string_value("shop-cert")));
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	EXPECT(run_function(&req, &rsp) == NULL);
	EXPECT(rsp.has_requirements && rsp.requirements.resources_count == 1);
	const fnv1_ResourceSelector *sel = &rsp.requirements.resources[0].value;
	EXPECT_STR("certificate", rsp.requirements.resources[0].key);
	EXPECT_STR("v1", sel->api_version);
	EXPECT_STR("Secret", sel->kind);
	EXPECT(sel->which_match == fnv1_ResourceSelector_match_name_tag);
	EXPECT_STR("shop-cert", sel->match.match_name);
	EXPECT_STR("team-a", sel->namespace);
	EXPECT(composed_secret(&rsp) == NULL);
	EXPECT(rsp.results_count == 0);
}

static void test_absent_secret_is_issued(void) {
	fnv1_RunFunctionRequest req = xr(empty());
	answered(&req, NULL);
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	EXPECT(run_function(&req, &rsp) == NULL);
	// The ask is repeated, the Secret composed under the default name.
	EXPECT(rsp.has_requirements && rsp.requirements.resources_count == 1);
	EXPECT_STR("shop-tls", rsp.requirements.resources[0].value.match.match_name);
	const google_protobuf_Struct *secret = composed_secret(&rsp);
	EXPECT(secret != NULL);
	if (!secret) {
		return;
	}
	EXPECT_STR("kubernetes.io/tls", structpb_string(structpb_get(secret, "type")));
	EXPECT_STR("shop-tls", structpb_string(structpb_lookup(secret, "metadata", "name", NULL)));
	const char *crt = structpb_string(structpb_lookup(secret, "data", "tls.crt", NULL));
	EXPECT(crt && strncmp(crt, certificate_prefix, strlen(certificate_prefix)) == 0);
	EXPECT(structpb_string(structpb_lookup(secret, "data", "tls.key", NULL)) != NULL);
	const google_protobuf_Struct *status = status_certificate(&rsp);
	EXPECT(status != NULL);
	if (status) {
		EXPECT_STR("shop-tls", structpb_string(structpb_get(status, "secretName")));
		EXPECT_STR("absent", structpb_string(structpb_get(status, "reason")));
		EXPECT_STR("CN=shop", structpb_string(structpb_get(status, "subject")));
		const google_protobuf_Value *rotated = structpb_get(status, "rotated");
		EXPECT(rotated && rotated->kind.bool_value);
	}
	EXPECT(rsp.results_count == 1 && rsp.results[0].severity == fnv1_Severity_SEVERITY_NORMAL);
	EXPECT(rsp.results_count == 1 && strncmp(rsp.results[0].message, "rotated the certificate in Secret shop-tls (absent)", 51) == 0);
	EXPECT(rsp.conditions_count == 1 && strcmp(rsp.conditions[0].reason, "Rotated") == 0);
}

static void test_valid_secret_is_carried_forward(void) {
	// What a previous run issued, as the Secret now holds it.
	fnv1_RunFunctionRequest first = xr(empty());
	answered(&first, NULL);
	fnv1_RunFunctionResponse issued = fnv1_RunFunctionResponse_init_zero;
	EXPECT(run_function(&first, &issued) == NULL);
	const google_protobuf_Struct *secret = composed_secret(&issued);
	EXPECT(secret != NULL);
	if (!secret) {
		return;
	}

	// An earlier step's status and desired resource survive.
	fnv1_RunFunctionRequest req = xr(object("commonName", structpb_string_value("shop.example.org")));
	answered(&req, secret);
	req.has_desired = true;
	req.desired.has_composite = true;
	req.desired.composite.has_resource = true;
	req.desired.composite.resource = object("status", structpb_struct_value(object("earlier", structpb_string_value("kept"))));
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	EXPECT(run_function(&req, &rsp) == NULL);
	const google_protobuf_Struct *carried = composed_secret(&rsp);
	EXPECT(carried != NULL);
	if (carried) {
		EXPECT_STR(structpb_string(structpb_lookup(secret, "data", "tls.crt", NULL)), structpb_string(structpb_lookup(carried, "data", "tls.crt", NULL)));
		EXPECT_STR(structpb_string(structpb_lookup(secret, "data", "tls.key", NULL)), structpb_string(structpb_lookup(carried, "data", "tls.key", NULL)));
	}
	const google_protobuf_Struct *status = status_certificate(&rsp);
	EXPECT(status != NULL);
	if (status) {
		EXPECT_STR("valid", structpb_string(structpb_get(status, "reason")));
		const google_protobuf_Value *rotated = structpb_get(status, "rotated");
		EXPECT(rotated && !rotated->kind.bool_value);
	}
	EXPECT_STR("kept", structpb_string(structpb_lookup(&rsp.desired.composite.resource, "status", "earlier", NULL)));
	EXPECT(rsp.results_count == 1 && strncmp(rsp.results[0].message, "the certificate in Secret shop-tls is valid until ", 50) == 0);
	EXPECT(rsp.conditions_count == 1 && strcmp(rsp.conditions[0].reason, "Valid") == 0);
}

static void test_unreadable_secret_is_replaced(void) {
	google_protobuf_Struct secret = object("data", structpb_struct_value(object("tls.crt", structpb_string_value("bm90IGEgY2VydGlmaWNhdGU="))));
	fnv1_RunFunctionRequest req = xr(empty());
	answered(&req, &secret);
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	EXPECT(run_function(&req, &rsp) == NULL);
	const google_protobuf_Struct *status = status_certificate(&rsp);
	EXPECT(status != NULL);
	if (status) {
		const char *reason = structpb_string(structpb_get(status, "reason"));
		EXPECT(reason && strncmp(reason, "unreadable: ", 12) == 0);
	}
}

static void test_spec_is_checked(void) {
	google_protobuf_Struct spec = object("validityDays", structpb_number_value(10));
	EXPECT(structpb_set(&spec, "renewBeforeDays", structpb_number_value(10)));
	fnv1_RunFunctionRequest req = xr(spec);
	fnv1_RunFunctionResponse rsp = fnv1_RunFunctionResponse_init_zero;
	EXPECT_STR("spec.renewBeforeDays must be at least 0 and less than spec.validityDays, or every run would rotate", run_function(&req, &rsp));

	fnv1_RunFunctionRequest bad_names = xr(object("dnsNames", structpb_string_value("not-a-list")));
	EXPECT_STR("cannot read spec.dnsNames: it must be a list of strings", run_function(&bad_names, &rsp));

	// A cluster-scoped composite resource cannot own a namespaced Secret.
	fnv1_RunFunctionRequest cluster = xr(empty());
	google_protobuf_Struct metadata = object("name", structpb_string_value("shop"));
	EXPECT(structpb_set(&cluster.observed.composite.resource, "metadata", structpb_struct_value(metadata)));
	const char *err = run_function(&cluster, &rsp);
	EXPECT(err && strncmp(err, "the composite resource has no namespace", 39) == 0);
}

int main(void) {
	test_no_composite();
	test_first_call_asks_for_the_secret();
	test_absent_secret_is_issued();
	test_valid_secret_is_carried_forward();
	test_unreadable_secret_is_replaced();
	test_spec_is_checked();
	if (failures) {
		fprintf(stderr, "%d failure(s)\n", failures);
		return 1;
	}
	puts("ok");
	return 0;
}
