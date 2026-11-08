// The certificate work of the function-wasm-cpp guest over mbedTLS: reading the
// validity of a PEM certificate, issuing a self-signed one over a fresh
// P-256 key, and the base64 a Kubernetes Secret's data speaks. Nothing here
// knows about Crossplane; rotator.cpp decides, this file does the crypto.
#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace tls {

// A certificate as the rotator reads it: who it is for, its serial, and its
// validity as Unix seconds (UTC).
struct Certificate {
	std::string subject;
	std::string serial;
	int64_t not_before = 0;
	int64_t not_after = 0;
};

// What issue() returns: the certificate and its key, both PEM.
struct Issued {
	std::string certificate_pem;
	std::string key_pem;
	Certificate certificate;
};

// parse reads the first certificate of pem. On failure it returns nothing
// and sets err to mbedTLS's words.
std::optional<Certificate> parse(const std::string &pem, std::string &err);

// issue generates a P-256 key and a self-signed certificate for common_name
// with dns_names as subject alternative names, valid from now for
// validity_days, signed with SHA-256, a random serial. On failure it returns
// nothing and sets err.
std::optional<Issued> issue(const std::string &common_name, const std::vector<std::string> &dns_names, int64_t now, int validity_days, std::string &err);

// base64_encode and base64_decode are the standard alphabet with padding, as
// a Secret's data carries its values.
std::string base64_encode(const std::string &raw);
std::optional<std::string> base64_decode(const std::string &encoded);

// rfc3339 formats Unix seconds as 2026-11-05T17:30:00Z.
std::string rfc3339(int64_t unix_seconds);

} // namespace tls
