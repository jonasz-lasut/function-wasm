/* See stdlib.h: memcpy comes from Zig's compiler-rt, strlen from src/wasmfn.zig. */
#ifndef WASMFN_LIBC_SHIM_STRING_H
#define WASMFN_LIBC_SHIM_STRING_H
#include <stddef.h>
void *memcpy(void *dst, const void *src, size_t n);
size_t strlen(const char *s);
#endif
