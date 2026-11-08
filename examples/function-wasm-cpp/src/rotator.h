// The decision of the function-wasm-cpp guest, behind a C interface: fn.c reads
// the request and builds the response over the generated protobuf structs
// (whose ResourceSelector carries a field named `namespace`, which no C++
// translation unit can include), and hands the facts to rotator_decide,
// C++ over tls.h. The two halves meet on plain C strings, every one heap
// allocated and never freed: a fresh instance serves each request.
#ifndef ROTATOR_H
#define ROTATOR_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// What the composite resource asks for and what Crossplane answered about
// the Secret.
typedef struct rotator_request {
	const char *common_name;
	const char *const *dns_names;
	size_t dns_names_count;
	int validity_days;
	int renew_before_days;
	// The Secret the read returned: found or not, and its data as the
	// Secret carries it (base64), NULL where a key is absent.
	bool secret_found;
	const char *certificate_b64;
	const char *key_b64;
	// The clock, Unix seconds.
	int64_t now;
} rotator_request;

// What to compose and report.
typedef struct rotator_result {
	// A new certificate and key were issued; otherwise the Secret's own data
	// is carried forward below.
	bool rotated;
	// Why: valid, absent, expired, expiring, or unreadable: <reason>.
	const char *reason;
	const char *certificate_b64;
	const char *key_b64;
	// The certificate in the Secret after this run.
	const char *subject;
	const char *serial;
	const char *not_before; // RFC 3339
	const char *not_after;
	int64_t seconds_left;
} rotator_result;

// rotator_decide fills out and returns NULL, or returns the message of a
// fatal result (issuing failed) and leaves out unspecified.
const char *rotator_decide(const rotator_request *req, rotator_result *out);

#ifdef __cplusplus
}
#endif

#endif
