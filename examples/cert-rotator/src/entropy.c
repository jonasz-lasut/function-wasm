// The entropy source mbedTLS draws from (MBEDTLS_ENTROPY_HARDWARE_ALT in
// cert_rotator_mbedtls_config.h): the host's randomness through getentropy,
// which wasi-libc serves from WASI's random interface and the host's libc
// from the kernel, so the native tests and the module generate keys the
// same way.
#include <stddef.h>
#include <unistd.h>

#if defined(__APPLE__) || defined(__linux__)
#include <sys/random.h>
#endif

int mbedtls_hardware_poll(void *data, unsigned char *output, size_t len, size_t *olen) {
	(void)data;
	*olen = 0;
	// getentropy serves at most 256 bytes per call.
	while (len > 0) {
		size_t chunk = len > 256 ? 256 : len;
		if (getentropy(output, chunk) != 0) {
			return -1;
		}
		output += chunk;
		len -= chunk;
		*olen += chunk;
	}
	return 0;
}
