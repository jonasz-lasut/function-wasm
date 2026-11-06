// The rotation decision (see rotator.h): read the certificate the Secret
// holds, keep it while it has more than renewBeforeDays left, issue a fresh
// self-signed one otherwise.
#include "rotator.h"

#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "tls.h"

namespace {

// keep hands a string to the C side: heap memory that outlives this call
// and is never freed, as the guest's memory discipline has it.
const char *keep(const std::string &s) {
	char *out = static_cast<char *>(std::malloc(s.size() + 1));
	if (!out) {
		return "";
	}
	std::memcpy(out, s.c_str(), s.size() + 1);
	return out;
}

void report(rotator_result *out, const tls::Certificate &c, int64_t now) {
	out->subject = keep(c.subject);
	out->serial = keep(c.serial);
	out->not_before = keep(tls::rfc3339(c.not_before));
	out->not_after = keep(tls::rfc3339(c.not_after));
	out->seconds_left = c.not_after - now;
}

// The certificate the Secret holds, or the reason it cannot be read.
std::optional<tls::Certificate> held(const rotator_request *req, std::string &why) {
	if (!req->secret_found) {
		why = "absent";
		return std::nullopt;
	}
	if (!req->certificate_b64 || !*req->certificate_b64) {
		why = "unreadable: the Secret has no tls.crt";
		return std::nullopt;
	}
	const std::optional<std::string> pem = tls::base64_decode(req->certificate_b64);
	if (!pem) {
		why = "unreadable: tls.crt is not base64";
		return std::nullopt;
	}
	std::string err;
	std::optional<tls::Certificate> c = tls::parse(*pem, err);
	if (!c) {
		why = "unreadable: " + err;
		return std::nullopt;
	}
	return c;
}

} // namespace

extern "C" const char *rotator_decide(const rotator_request *req, rotator_result *out) {
	*out = rotator_result{};
	std::string why;
	const std::optional<tls::Certificate> current = held(req, why);
	if (current) {
		const int64_t left = current->not_after - req->now;
		if (left > static_cast<int64_t>(req->renew_before_days) * 86400) {
			out->rotated = false;
			out->reason = "valid";
			out->certificate_b64 = req->certificate_b64;
			out->key_b64 = req->key_b64 ? req->key_b64 : "";
			report(out, *current, req->now);
			return nullptr;
		}
		why = left <= 0 ? "expired" : "expiring";
	}

	std::vector<std::string> dns_names;
	for (size_t i = 0; i < req->dns_names_count; i++) {
		dns_names.emplace_back(req->dns_names[i]);
	}
	std::string err;
	const std::optional<tls::Issued> issued = tls::issue(req->common_name, dns_names, req->now, req->validity_days, err);
	if (!issued) {
		return keep("cannot issue a certificate for " + std::string(req->common_name) + ": " + err);
	}
	out->rotated = true;
	out->reason = keep(why);
	out->certificate_b64 = keep(tls::base64_encode(issued->certificate_pem));
	out->key_b64 = keep(tls::base64_encode(issued->key_pem));
	report(out, issued->certificate, req->now);
	return nullptr;
}
