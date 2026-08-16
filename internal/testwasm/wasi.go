package testwasm

import (
	"fmt"
	"strings"
)

// WASI fixtures exercise the sandbox through the raw wasi_snapshot_preview1
// imports, so the host's pre-opens and environment are checked without a
// language runtime in between. Each one hands the bytes it obtained back as
// the message of a normal Result: the response is
//
//	RunFunctionResponse{Results: [{Severity: NORMAL, Message: <bytes>}]}
//
// framed by hand in guest memory ($result below), which keeps the payload
// under 124 bytes so every length fits one protobuf varint byte. A WASI
// error ends the run with proc_exit(errno), so the host reports
// "module exited with status <errno>" — the errno is the assertion.
//
// Pre-opened directories are addressed by descriptor: WASI numbers them from
// 3 in the order the host pre-opened them, so a fixture opening "name" on fd
// 3 reads the private /tmp — the only pre-open a guest ever gets.

// Guest memory used by the WASI fixtures, away from testwasm's canned
// response (1024) and heap (65536): scratch words, the environ pointer
// table and the framed Result.
const (
	wasiPathOffset    = 16   // the file name a fixture opens
	wasiContentOffset = 256  // the bytes WriteRead writes
	wasiIOVOffset     = 4096 // one iovec {ptr, len}
	wasiSizeOffset    = 4104 // nread / nwritten / environ count
	wasiFdOffset      = 4108 // the descriptor path_open returned
	wasiSize2Offset   = 4112 // environ buffer size
	wasiEnvironOffset = 6144 // environ pointer table (room for 512 entries)
	wasiResultOffset  = 8192 // the framed Result: 6 header bytes, then the payload
	wasiPayloadOffset = wasiResultOffset + 6
	wasiPayloadMax    = 123 // keeps every varint in the frame to one byte
)

// WASI constants the fixtures pass to path_open.
const (
	// wasiRightsFDRead and wasiRightsFDWrite are the fd rights path_open
	// asks for; wasmtime opens the file for reading and/or writing
	// accordingly.
	wasiRightsFDRead  = 1 << 1
	wasiRightsFDWrite = 1 << 6
	// wasiOflagsCreate|wasiOflagsTrunc create or empty a file on open.
	wasiOflagsCreate = 1
	wasiOflagsTrunc  = 8
	// wasiPreopenFD is the first pre-opened directory.
	wasiPreopenFD = 3
)

// wasiImports declares the WASI functions the fixtures use and the $result
// helper that frames the payload at wasiPayloadOffset as a normal Result and
// returns the packed pointer/length wasmfn_run must return.
const wasiImports = `(import "wasi_snapshot_preview1" "path_open" (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "environ_sizes_get" (func $environ_sizes_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "environ_get" (func $environ_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  ;; $check exits with the errno of a failed WASI call.
  (func $check (param $errno i32)
    (if (local.get $errno) (then (call $proc_exit (local.get $errno)))))
  ;; $result frames $n payload bytes at %[2]d as
  ;; results[0] {severity: NORMAL, message: payload}: field 3 length-delimited,
  ;; then field 1 varint 3 and field 2 length-delimited.
  (func $result (param $n i32) (result i64)
    (if (i32.gt_u (local.get $n) (i32.const %[3]d)) (then unreachable))
    (i32.store8 offset=0 (i32.const %[1]d) (i32.const 0x1a))
    (i32.store8 offset=1 (i32.const %[1]d) (i32.add (local.get $n) (i32.const 4)))
    (i32.store8 offset=2 (i32.const %[1]d) (i32.const 0x08))
    (i32.store8 offset=3 (i32.const %[1]d) (i32.const 0x03))
    (i32.store8 offset=4 (i32.const %[1]d) (i32.const 0x12))
    (i32.store8 offset=5 (i32.const %[1]d) (local.get $n))
    (i64.or (i64.shl (i64.const %[1]d) (i64.const 32)) (i64.extend_i32_u (i32.add (local.get $n) (i32.const 6)))))
`

func wasiExtra(name string, content string) string {
	extra := fmt.Sprintf(wasiImports, wasiResultOffset, wasiPayloadOffset, wasiPayloadMax)
	if name != "" {
		extra += fmt.Sprintf("  (data (i32.const %d) \"%s\")\n", wasiPathOffset, escape([]byte(name)))
	}
	if content != "" {
		extra += fmt.Sprintf("  (data (i32.const %d) \"%s\")\n", wasiContentOffset, escape([]byte(content)))
	}
	return extra
}

// openWAT opens name (at wasiPathOffset) on the first pre-open with oflags
// and rights, leaving the descriptor at wasiFdOffset.
func openWAT(name string, oflags, rights int) string {
	return fmt.Sprintf("(call $check (call $path_open (i32.const %d) (i32.const 0) (i32.const %d) (i32.const %d) (i32.const %d) (i64.const %d) (i64.const 0) (i32.const 0) (i32.const %d)))",
		wasiPreopenFD, wasiPathOffset, len(name), oflags, rights, wasiFdOffset)
}

// iovWAT sets the single iovec to {ptr, len}.
func iovWAT(ptr, n int) string {
	return fmt.Sprintf("(i32.store (i32.const %d) (i32.const %d)) (i32.store (i32.const %d) (i32.const %d))", wasiIOVOffset, ptr, wasiIOVOffset+4, n)
}

// readWAT reads from the opened descriptor into the payload area and closes
// it; the byte count lands at wasiSizeOffset.
func readWAT() string {
	return strings.Join([]string{
		iovWAT(wasiPayloadOffset, wasiPayloadMax),
		fmt.Sprintf("(call $check (call $fd_read (i32.load (i32.const %d)) (i32.const %d) (i32.const 1) (i32.const %d)))", wasiFdOffset, wasiIOVOffset, wasiSizeOffset),
		fmt.Sprintf("(call $check (call $fd_close (i32.load (i32.const %d))))", wasiFdOffset),
	}, "\n    ")
}

// ReadFile returns Options for a module whose wasmfn_run opens name in the
// first pre-opened directory, reads it and returns its content as the message
// of a normal Result. name may contain "../" — the point of one test.
func ReadFile(name string) Options {
	return Options{
		Extra: wasiExtra(name, ""),
		Body: strings.Join([]string{
			openWAT(name, 0, wasiRightsFDRead),
			readWAT(),
			fmt.Sprintf("(call $result (i32.load (i32.const %d)))", wasiSizeOffset),
		}, "\n    "),
	}
}

// WriteRead returns Options for a module whose wasmfn_run creates name in the
// first pre-opened directory, writes content into it, closes it, opens it
// again and returns what it reads back — proof that the directory is
// writable and that a write is visible within the run.
func WriteRead(name, content string) Options {
	return Options{
		Extra: wasiExtra(name, content),
		Body: strings.Join([]string{
			openWAT(name, wasiOflagsCreate|wasiOflagsTrunc, wasiRightsFDWrite),
			iovWAT(wasiContentOffset, len(content)),
			fmt.Sprintf("(call $check (call $fd_write (i32.load (i32.const %d)) (i32.const %d) (i32.const 1) (i32.const %d)))", wasiFdOffset, wasiIOVOffset, wasiSizeOffset),
			fmt.Sprintf("(call $check (call $fd_close (i32.load (i32.const %d))))", wasiFdOffset),
			openWAT(name, 0, wasiRightsFDRead),
			readWAT(),
			fmt.Sprintf("(call $result (i32.load (i32.const %d)))", wasiSizeOffset),
		}, "\n    "),
	}
}

// Environ returns Options for a module whose wasmfn_run returns the guest's
// WASI environment as the message of a normal Result: "K=V\x00K2=V2\x00" in
// the host's order, empty when the guest has no environment.
func Environ() Options {
	return Options{
		Extra: wasiExtra("", ""),
		Body: strings.Join([]string{
			fmt.Sprintf("(call $check (call $environ_sizes_get (i32.const %d) (i32.const %d)))", wasiSizeOffset, wasiSize2Offset),
			fmt.Sprintf("(call $check (call $environ_get (i32.const %d) (i32.const %d)))", wasiEnvironOffset, wasiPayloadOffset),
			fmt.Sprintf("(call $result (i32.load (i32.const %d)))", wasiSize2Offset),
		}, "\n    "),
	}
}
