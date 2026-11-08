// The mbedTLS configuration of the function-wasm-cpp guest: X.509 parse and
// write, ECDSA over P-256, SHA-256, PEM and base64, CTR-DRBG seeded from
// the host's randomness - and nothing that needs a socket, a file or a
// clock the sandbox does not have. The legacy (non-PSA) API throughout.
#ifndef CERT_ROTATOR_MBEDTLS_CONFIG_H
#define CERT_ROTATOR_MBEDTLS_CONFIG_H

// Platform: libc's malloc, printf and friends; no file I/O, no time
// functions from mbedTLS itself (the guest compares validity dates on its
// own), entropy from mbedtls_hardware_poll (entropy.c, over getentropy).
#define MBEDTLS_PLATFORM_C
#define MBEDTLS_ERROR_C
#define MBEDTLS_NO_PLATFORM_ENTROPY
#define MBEDTLS_ENTROPY_HARDWARE_ALT
#define MBEDTLS_ENTROPY_C
#define MBEDTLS_CTR_DRBG_C
#define MBEDTLS_AES_C
#define MBEDTLS_SHA256_C
#define MBEDTLS_SHA224_C
#define MBEDTLS_MD_C

// Keys and signatures: ECDSA over secp256r1, with deterministic nonces.
#define MBEDTLS_BIGNUM_C
#define MBEDTLS_ECP_C
#define MBEDTLS_ECP_DP_SECP256R1_ENABLED
#define MBEDTLS_ECDSA_C
#define MBEDTLS_ECDSA_DETERMINISTIC
#define MBEDTLS_HMAC_DRBG_C
#define MBEDTLS_PK_C
#define MBEDTLS_PK_PARSE_C
#define MBEDTLS_PK_WRITE_C

// X.509: parse a certificate, write a self-signed one; PEM and base64 for
// the Secret's data.
#define MBEDTLS_ASN1_PARSE_C
#define MBEDTLS_ASN1_WRITE_C
#define MBEDTLS_OID_C
#define MBEDTLS_X509_USE_C
#define MBEDTLS_X509_CRT_PARSE_C
#define MBEDTLS_X509_CREATE_C
#define MBEDTLS_X509_CRT_WRITE_C
#define MBEDTLS_PEM_PARSE_C
#define MBEDTLS_PEM_WRITE_C
#define MBEDTLS_BASE64_C

#endif
