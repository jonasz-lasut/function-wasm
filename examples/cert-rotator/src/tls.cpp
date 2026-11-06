// The certificate work over mbedTLS (see tls.h). Every mbedTLS context is
// freed before a function returns, unlike the guest's other allocations: the
// library zeroes key material on free, which a fresh instance per request
// does not do for us.
#include "tls.h"

#include <mbedtls/asn1.h>
#include <mbedtls/base64.h>
#include <mbedtls/ctr_drbg.h>
#include <mbedtls/ecp.h>
#include <mbedtls/entropy.h>
#include <mbedtls/error.h>
#include <mbedtls/oid.h>
#include <mbedtls/pk.h>
#include <mbedtls/x509.h>
#include <mbedtls/x509_crt.h>

#include <cstdio>
#include <cstring>

namespace tls {

namespace {

// words is mbedTLS's description of an error code, for a message a human
// reads in a fatal result.
std::string words(int code) {
	char buf[160];
	mbedtls_strerror(code, buf, sizeof buf);
	return buf;
}

// The proleptic Gregorian calendar as days since 1970-01-01, after Howard
// Hinnant's algorithms: what the x509 validity dates and the RFC 3339
// stamps are converted through, with no dependence on the libc's time zone.
int64_t days_from_civil(int64_t y, unsigned m, unsigned d) {
	y -= m <= 2;
	const int64_t era = (y >= 0 ? y : y - 399) / 400;
	const unsigned yoe = static_cast<unsigned>(y - era * 400);
	const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
	const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	return era * 146097 + static_cast<int64_t>(doe) - 719468;
}

void civil_from_days(int64_t z, int64_t &y, unsigned &m, unsigned &d) {
	z += 719468;
	const int64_t era = (z >= 0 ? z : z - 146096) / 146097;
	const unsigned doe = static_cast<unsigned>(z - era * 146097);
	const unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
	y = static_cast<int64_t>(yoe) + era * 400;
	const unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	const unsigned mp = (5 * doy + 2) / 153;
	d = doy - (153 * mp + 2) / 5 + 1;
	m = mp + (mp < 10 ? 3 : -9);
	y += m <= 2;
}

int64_t to_unix(const mbedtls_x509_time &t) {
	return days_from_civil(t.year, static_cast<unsigned>(t.mon), static_cast<unsigned>(t.day)) * 86400 + t.hour * 3600 + t.min * 60 + t.sec;
}

struct Civil {
	int64_t year;
	unsigned month, day, hour, minute, second;
};

Civil civil(int64_t unix_seconds) {
	int64_t days = unix_seconds / 86400;
	int64_t rem = unix_seconds % 86400;
	if (rem < 0) {
		rem += 86400;
		days -= 1;
	}
	Civil c{};
	civil_from_days(days, c.year, c.month, c.day);
	c.hour = static_cast<unsigned>(rem / 3600);
	c.minute = static_cast<unsigned>(rem % 3600 / 60);
	c.second = static_cast<unsigned>(rem % 60);
	return c;
}

// stamp is the YYYYMMDDHHMMSS form the x509 writer takes for validity.
std::string stamp(int64_t unix_seconds) {
	const Civil c = civil(unix_seconds);
	char buf[32];
	std::snprintf(buf, sizeof buf, "%04lld%02u%02u%02u%02u%02u", static_cast<long long>(c.year), c.month, c.day, c.hour, c.minute, c.second);
	return buf;
}

// A distinguished-name value with the characters mbedTLS's DN parser gives
// a meaning to escaped (RFC 4514).
std::string escaped(const std::string &value) {
	std::string out;
	for (char ch : value) {
		if (std::strchr(",=+<>#;\"\\", ch)) {
			out += '\\';
		}
		out += ch;
	}
	return out;
}

Certificate read(const mbedtls_x509_crt &crt) {
	Certificate c;
	char buf[512];
	if (mbedtls_x509_dn_gets(buf, sizeof buf, &crt.subject) > 0) {
		c.subject = buf;
	}
	if (mbedtls_x509_serial_gets(buf, sizeof buf, &crt.serial) > 0) {
		c.serial = buf;
	}
	c.not_before = to_unix(crt.valid_from);
	c.not_after = to_unix(crt.valid_to);
	return c;
}

// Everything issue() holds, freed on every path out.
struct Issuer {
	mbedtls_entropy_context entropy;
	mbedtls_ctr_drbg_context drbg;
	mbedtls_pk_context key;
	mbedtls_x509write_cert crt;

	Issuer() {
		mbedtls_entropy_init(&entropy);
		mbedtls_ctr_drbg_init(&drbg);
		mbedtls_pk_init(&key);
		mbedtls_x509write_crt_init(&crt);
	}
	~Issuer() {
		mbedtls_x509write_crt_free(&crt);
		mbedtls_pk_free(&key);
		mbedtls_ctr_drbg_free(&drbg);
		mbedtls_entropy_free(&entropy);
	}
	Issuer(const Issuer &) = delete;
	Issuer &operator=(const Issuer &) = delete;
};

} // namespace

std::optional<Certificate> parse(const std::string &pem, std::string &err) {
	mbedtls_x509_crt crt;
	mbedtls_x509_crt_init(&crt);
	// A PEM buffer must be NUL-terminated, the terminator counted.
	const int rc = mbedtls_x509_crt_parse(&crt, reinterpret_cast<const unsigned char *>(pem.c_str()), pem.size() + 1);
	if (rc != 0) {
		err = words(rc);
		mbedtls_x509_crt_free(&crt);
		return std::nullopt;
	}
	Certificate c = read(crt);
	mbedtls_x509_crt_free(&crt);
	return c;
}

std::optional<Issued> issue(const std::string &common_name, const std::vector<std::string> &dns_names, int64_t now, int validity_days, std::string &err) {
	Issuer it;
	const char *personalization = "function-wasm cert-rotator";
	int rc = mbedtls_ctr_drbg_seed(&it.drbg, mbedtls_entropy_func, &it.entropy, reinterpret_cast<const unsigned char *>(personalization), std::strlen(personalization));
	if (rc != 0) {
		err = "cannot seed the random generator: " + words(rc);
		return std::nullopt;
	}
	rc = mbedtls_pk_setup(&it.key, mbedtls_pk_info_from_type(MBEDTLS_PK_ECKEY));
	if (rc == 0) {
		rc = mbedtls_ecp_gen_key(MBEDTLS_ECP_DP_SECP256R1, mbedtls_pk_ec(it.key), mbedtls_ctr_drbg_random, &it.drbg);
	}
	if (rc != 0) {
		err = "cannot generate the key: " + words(rc);
		return std::nullopt;
	}

	mbedtls_x509write_crt_set_version(&it.crt, MBEDTLS_X509_CRT_VERSION_3);
	mbedtls_x509write_crt_set_md_alg(&it.crt, MBEDTLS_MD_SHA256);
	mbedtls_x509write_crt_set_subject_key(&it.crt, &it.key);
	mbedtls_x509write_crt_set_issuer_key(&it.crt, &it.key);
	const std::string dn = "CN=" + escaped(common_name);
	rc = mbedtls_x509write_crt_set_subject_name(&it.crt, dn.c_str());
	if (rc == 0) {
		rc = mbedtls_x509write_crt_set_issuer_name(&it.crt, dn.c_str());
	}
	if (rc != 0) {
		err = "cannot set the subject " + dn + ": " + words(rc);
		return std::nullopt;
	}
	unsigned char serial[16];
	rc = mbedtls_ctr_drbg_random(&it.drbg, serial, sizeof serial);
	serial[0] &= 0x7f; // a positive integer, as RFC 5280 asks
	if (rc == 0) {
		rc = mbedtls_x509write_crt_set_serial_raw(&it.crt, serial, sizeof serial);
	}
	if (rc != 0) {
		err = "cannot set the serial: " + words(rc);
		return std::nullopt;
	}
	const std::string not_before = stamp(now);
	const std::string not_after = stamp(now + static_cast<int64_t>(validity_days) * 86400);
	rc = mbedtls_x509write_crt_set_validity(&it.crt, not_before.c_str(), not_after.c_str());
	if (rc == 0) {
		rc = mbedtls_x509write_crt_set_basic_constraints(&it.crt, 0, -1);
	}
	if (rc == 0) {
		rc = mbedtls_x509write_crt_set_key_usage(&it.crt, MBEDTLS_X509_KU_DIGITAL_SIGNATURE | MBEDTLS_X509_KU_KEY_ENCIPHERMENT);
	}
	if (rc == 0) {
		mbedtls_asn1_sequence server_auth{};
		server_auth.buf.tag = MBEDTLS_ASN1_OID;
		server_auth.buf.len = MBEDTLS_OID_SIZE(MBEDTLS_OID_SERVER_AUTH);
		server_auth.buf.p = reinterpret_cast<unsigned char *>(const_cast<char *>(MBEDTLS_OID_SERVER_AUTH));
		rc = mbedtls_x509write_crt_set_ext_key_usage(&it.crt, &server_auth);
	}
	if (rc != 0) {
		err = "cannot set the extensions: " + words(rc);
		return std::nullopt;
	}

	// The subject alternative names: the DNS names asked for, the common name
	// among them (clients match the SANs, not the CN).
	std::vector<std::string> names = dns_names;
	bool listed = false;
	for (const auto &n : names) {
		listed = listed || n == common_name;
	}
	if (!listed) {
		names.insert(names.begin(), common_name);
	}
	std::vector<mbedtls_x509_san_list> sans(names.size());
	for (size_t i = 0; i < names.size(); i++) {
		sans[i] = mbedtls_x509_san_list{};
		sans[i].node.type = MBEDTLS_X509_SAN_DNS_NAME;
		sans[i].node.san.unstructured_name.tag = MBEDTLS_ASN1_IA5_STRING;
		sans[i].node.san.unstructured_name.len = names[i].size();
		sans[i].node.san.unstructured_name.p = reinterpret_cast<unsigned char *>(const_cast<char *>(names[i].c_str()));
		sans[i].next = i + 1 < names.size() ? &sans[i + 1] : nullptr;
	}
	rc = mbedtls_x509write_crt_set_subject_alternative_name(&it.crt, sans.data());
	if (rc != 0) {
		err = "cannot set the subject alternative names: " + words(rc);
		return std::nullopt;
	}

	std::string certificate_pem(4096, '\0');
	rc = mbedtls_x509write_crt_pem(&it.crt, reinterpret_cast<unsigned char *>(certificate_pem.data()), certificate_pem.size(), mbedtls_ctr_drbg_random, &it.drbg);
	if (rc != 0) {
		err = "cannot write the certificate: " + words(rc);
		return std::nullopt;
	}
	certificate_pem.resize(std::strlen(certificate_pem.c_str()));
	std::string key_pem(2048, '\0');
	rc = mbedtls_pk_write_key_pem(&it.key, reinterpret_cast<unsigned char *>(key_pem.data()), key_pem.size());
	if (rc != 0) {
		err = "cannot write the key: " + words(rc);
		return std::nullopt;
	}
	key_pem.resize(std::strlen(key_pem.c_str()));

	// Read the certificate back: what the rotator reports is what it wrote.
	std::string parse_err;
	std::optional<Certificate> written = parse(certificate_pem, parse_err);
	if (!written) {
		err = "cannot read the certificate back: " + parse_err;
		return std::nullopt;
	}
	return Issued{certificate_pem, key_pem, *written};
}

std::string base64_encode(const std::string &raw) {
	size_t needed = 0;
	const auto *src = reinterpret_cast<const unsigned char *>(raw.data());
	mbedtls_base64_encode(nullptr, 0, &needed, src, raw.size());
	std::string out(needed, '\0');
	size_t written = 0;
	if (mbedtls_base64_encode(reinterpret_cast<unsigned char *>(out.data()), out.size(), &written, src, raw.size()) != 0) {
		return "";
	}
	out.resize(written);
	return out;
}

std::optional<std::string> base64_decode(const std::string &encoded) {
	size_t needed = 0;
	const auto *src = reinterpret_cast<const unsigned char *>(encoded.data());
	const int probe = mbedtls_base64_decode(nullptr, 0, &needed, src, encoded.size());
	if (probe == MBEDTLS_ERR_BASE64_INVALID_CHARACTER) {
		return std::nullopt;
	}
	std::string out(needed, '\0');
	size_t written = 0;
	if (mbedtls_base64_decode(reinterpret_cast<unsigned char *>(out.data()), out.size(), &written, src, encoded.size()) != 0) {
		return std::nullopt;
	}
	out.resize(written);
	return out;
}

std::string rfc3339(int64_t unix_seconds) {
	const Civil c = civil(unix_seconds);
	char buf[40];
	std::snprintf(buf, sizeof buf, "%04lld-%02u-%02uT%02u:%02u:%02uZ", static_cast<long long>(c.year), c.month, c.day, c.hour, c.minute, c.second);
	return buf;
}

} // namespace tls
