/* The libc wit-bindgen's generated C expects, declared only: src/wasmfn.zig
 * defines these over the guest's bump heap, so the core module links no
 * libc and the component needs no wasip1 adapter. */
#ifndef WASMFN_LIBC_SHIM_STDLIB_H
#define WASMFN_LIBC_SHIM_STDLIB_H
#include <stddef.h>
void *realloc(void *ptr, size_t size);
void free(void *ptr);
void abort(void) __attribute__((noreturn));
#endif
