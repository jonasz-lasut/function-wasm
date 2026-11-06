// Native unit tests of the decision and the crypto: zig build test.
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "rotator.h"
#include "tls.h"

static int failures;

#define EXPECT(cond) expect(__LINE__, (cond), #cond)
#define EXPECT_EQ(want, got) expect_eq(__LINE__, (want), (got))

static void expect(int line, bool ok, const char *what) {
	if (!ok) {
		std::fprintf(stderr, "rotator_test.cpp:%d: %s is false\n", line, what);
		failures++;
	}
}

static void expect_eq(int line, const std::string &want, const std::string &got) {
	if (want != got) {
		std::fprintf(stderr, "rotator_test.cpp:%d: want %s, got %s\n", line, want.c_str(), got.c_str());
		failures++;
	}
}

// 2026-11-05T12:00:00Z.
static const int64_t now = 1793880000;

static void test_times() {
	EXPECT_EQ("1970-01-01T00:00:00Z", tls::rfc3339(0));
	EXPECT_EQ("2026-11-05T12:00:00Z", tls::rfc3339(now));
	EXPECT_EQ("2000-02-29T23:59:59Z", tls::rfc3339(951868799));
}

static void test_base64() {
	EXPECT_EQ("aGVsbG8=", tls::base64_encode("hello"));
	EXPECT_EQ("", tls::base64_encode(""));
	const auto decoded = tls::base64_decode("aGVsbG8=");
	EXPECT(decoded && *decoded == "hello");
	EXPECT(!tls::base64_decode("not base64!"));
}

static void test_issue_and_parse() {
	std::string err;
	const auto issued = tls::issue("shop.example.org", {"www.shop.example.org"}, now, 90, err);
	EXPECT(issued.has_value());
	if (!issued) {
		std::fprintf(stderr, "issue: %s\n", err.c_str());
		return;
	}
	EXPECT(issued->certificate_pem.rfind("-----BEGIN CERTIFICATE-----", 0) == 0);
	EXPECT(issued->key_pem.rfind("-----BEGIN EC PRIVATE KEY-----", 0) == 0);
	EXPECT_EQ("CN=shop.example.org", issued->certificate.subject);
	EXPECT(issued->certificate.not_before == now);
	EXPECT(issued->certificate.not_after == now + 90 * 86400);
	EXPECT(!issued->certificate.serial.empty());

	const auto parsed = tls::parse(issued->certificate_pem, err);
	EXPECT(parsed.has_value());
	if (parsed) {
		EXPECT_EQ(issued->certificate.serial, parsed->serial);
		EXPECT(parsed->not_after == now + 90 * 86400);
	}
	// Two issues differ: a fresh key and a random serial each time.
	const auto again = tls::issue("shop.example.org", {}, now, 90, err);
	EXPECT(again && again->key_pem != issued->key_pem && again->certificate.serial != issued->certificate.serial);
	// A common name with a comma is escaped into the DN, not split.
	const auto odd = tls::issue("shop, the one", {}, now, 1, err);
	EXPECT(odd.has_value());
	if (odd) {
		EXPECT_EQ("CN=shop\\, the one", odd->certificate.subject);
	}
	EXPECT(!tls::parse("-----BEGIN CERTIFICATE-----\nnope\n-----END CERTIFICATE-----\n", err));
	EXPECT(!err.empty());
}

static rotator_request request() {
	rotator_request r{};
	r.common_name = "shop.example.org";
	r.validity_days = 90;
	r.renew_before_days = 30;
	r.now = now;
	return r;
}

static void test_decide() {
	// Absent: issued.
	rotator_request absent = request();
	rotator_result fresh{};
	EXPECT(rotator_decide(&absent, &fresh) == nullptr);
	EXPECT(fresh.rotated);
	EXPECT_EQ("absent", fresh.reason);
	EXPECT_EQ("2027-02-03T12:00:00Z", fresh.not_after);
	EXPECT(fresh.seconds_left == 90 * 86400);

	// Found and valid: carried forward, data untouched.
	rotator_request found = request();
	found.secret_found = true;
	found.certificate_b64 = fresh.certificate_b64;
	found.key_b64 = fresh.key_b64;
	found.now = now + 30 * 86400;
	rotator_result kept{};
	EXPECT(rotator_decide(&found, &kept) == nullptr);
	EXPECT(!kept.rotated);
	EXPECT_EQ("valid", kept.reason);
	EXPECT(std::strcmp(kept.certificate_b64, fresh.certificate_b64) == 0);
	EXPECT(std::strcmp(kept.key_b64, fresh.key_b64) == 0);
	EXPECT_EQ(fresh.serial, kept.serial);
	EXPECT(kept.seconds_left == 60 * 86400);

	// Within renewBeforeDays of the end: rotated, a new serial.
	found.now = now + 61 * 86400;
	rotator_result expiring{};
	EXPECT(rotator_decide(&found, &expiring) == nullptr);
	EXPECT(expiring.rotated);
	EXPECT_EQ("expiring", expiring.reason);
	EXPECT(std::strcmp(expiring.serial, fresh.serial) != 0);
	EXPECT_EQ("2027-04-05T12:00:00Z", expiring.not_after);

	// Past the end: expired.
	found.now = now + 91 * 86400;
	rotator_result expired{};
	EXPECT(rotator_decide(&found, &expired) == nullptr);
	EXPECT(expired.rotated);
	EXPECT_EQ("expired", expired.reason);

	// A Secret with no certificate, or one that is not a certificate.
	rotator_request empty = request();
	empty.secret_found = true;
	rotator_result replaced{};
	EXPECT(rotator_decide(&empty, &replaced) == nullptr);
	EXPECT(replaced.rotated);
	EXPECT_EQ("unreadable: the Secret has no tls.crt", replaced.reason);
	empty.certificate_b64 = "bm90IGEgY2VydGlmaWNhdGU="; // "not a certificate"
	EXPECT(rotator_decide(&empty, &replaced) == nullptr);
	EXPECT(replaced.rotated);
	EXPECT(std::string(replaced.reason).rfind("unreadable: ", 0) == 0);
}

int main() {
	test_times();
	test_base64();
	test_issue_and_parse();
	test_decide();
	if (failures) {
		std::fprintf(stderr, "%d failure(s)\n", failures);
		return 1;
	}
	std::puts("ok");
	return 0;
}
