// The function-wasm ABI v2 glue for an Odin guest (docs/abi-v2.md): the
// world's `run` export (decode the request, run `run_function`, encode the
// response; every failure becomes a fatal result), the typed `log` import as
// a logger and the host's HTTP egress behind one call. The component's
// surface is wit-bindgen's C bindings for wit/world.wit (src/gen, the c
// flavour's): their run shim calls the one C function they expect,
// `exports_function_run`, which this file defines, and their log import and
// wasi:http client are reached through Odin's foreign procedures. Only the
// export and the imports are wasm-specific: natively the logger prints to
// stderr and a test may install a fake host for HTTP, so the function builds
// and tests as ordinary Odin (zig build test).
//
// Memory: the canonical ABI frees with libc's free - the request buffer it
// lowered into our memory (cabi_realloc, freed once decoded) and the response
// buffer we hand back (freed by the generated cabi_post_run after the host
// has read it) - so the context's allocator is libc's heap, the one heap of
// the module: anything Odin allocates may be given to the C side and back.
// Nothing else is ever freed: a fresh instance serves each request and the
// host drops it afterwards.
package guest

import "base:runtime"
import "core:c"
import "core:strings"

IS_WASM :: ODIN_ARCH == .wasm32

// ─── the canonical ABI's types (src/gen/function.h) ─────────────────────────

// function_string_t and function_list_u8_t: {uint8_t *ptr; size_t len;}.
Bindings_String :: struct {
	ptr: [^]u8,
	len: c.size_t,
}

Bindings_List_U8 :: struct {
	ptr: [^]u8,
	len: c.size_t,
}

Bindings_Tuple2_String_String :: struct {
	f0: Bindings_String,
	f1: Bindings_String,
}

Bindings_List_Tuple2_String_String :: struct {
	ptr: [^]Bindings_Tuple2_String_String,
	len: c.size_t,
}

when IS_WASM {
	#assert(size_of(Bindings_String) == 8 && size_of(Bindings_Tuple2_String_String) == 16)

	// The generated C (src/gen/function.c): the world's log import behind
	// its shim, and the list free the run shim's argument wants after
	// decoding. A foreign import whose name ends in .o is, on a wasm
	// target, a set of plain link-time symbols (no wasm import module), and
	// Odin never opens the file: the names say where zig links them from.
	foreign import bindings "function.o"

	@(default_calling_convention = "c")
	foreign bindings {
		function_log :: proc(level: u8, msg: ^Bindings_String, kv: ^Bindings_List_Tuple2_String_String) ---
		function_list_u8_free :: proc(list: ^Bindings_List_U8) ---
	}

	// The C half of the glue (src/wasmfn.c): the wasi:http@0.2 client.
	foreign import wasmfn "wasmfn.o"

	@(default_calling_convention = "c")
	foreign wasmfn {
		wasmfn_http_get_text :: proc(url: cstring, err: ^cstring) -> cstring ---
	}

	// wasi-libc's heap, the module's one heap (see the top of this file).
	foreign import libc "libc.o"
} else {
	// Natively (the unit tests) the same procedures are the system libc's.
	foreign import libc "system:c"
}

@(default_calling_convention = "c")
foreign libc {
	malloc :: proc(size: c.size_t) -> rawptr ---
	realloc :: proc(ptr: rawptr, size: c.size_t) -> rawptr ---
	free :: proc(ptr: rawptr) ---
	aligned_alloc :: proc(alignment, size: c.size_t) -> rawptr ---
}

DEFAULT_TTL_SECONDS :: 60

// ─── the run export ─────────────────────────────────────────────────────────

when IS_WASM {
	// The world's run, behind the generated shim: the host lowered the
	// request bytes into a buffer of ours (cabi_realloc), the encoded
	// response goes back as an owned list the generated cabi_post_run frees
	// once the host has copied it out. An error string here becomes the
	// request's fatal result on the host's side; the glue only reaches it
	// when it cannot even encode a reply. @(require): nothing in Odin
	// references it; "strong" linkage makes it a plain defined symbol for
	// the shim (an @(export) would make it a wasm export of its own beside
	// the world's).
	@(require, linkage = "strong", link_name = "exports_function_run")
	exports_function_run :: proc "c" (request: ^Bindings_List_U8, ret: ^Bindings_List_U8, err: ^Bindings_String) -> bool {
		context = guest_context()
		input := request.ptr[:request.len]
		out, ok := handle(input)
		function_list_u8_free(request)
		if !ok {
			// Owned by the canonical ABI from here (cabi_post_run frees
			// it): a copy on the libc heap, not the literal.
			msg := strings.clone("cannot encode RunFunctionResponse")
			err^ = {raw_data(msg), c.size_t(len(msg))}
			return false
		}
		ret^ = {raw_data(out), c.size_t(len(out))}
		return true
	}
}

// guest_context is the context every Odin procedure runs under: the default
// one with its allocators on the libc heap. On freestanding wasm the default
// temp allocator is the nil allocator and a panic traps without a message
// (the runtime has nowhere to write it); the host reports the trap.
guest_context :: proc "contextless" () -> runtime.Context {
	ctx := runtime.default_context()
	ctx.allocator = libc_allocator()
	ctx.temp_allocator = ctx.allocator
	return ctx
}

// handle is the guest half of the contract: decode, run, encode. Every
// failure the guest can describe becomes a fatal result so the host can
// always decode the reply; only a response that cannot be encoded uses
// run's error.
handle :: proc(input: []u8) -> (out: []u8, ok: bool) {
	req: Run_Function_Request
	rsp: Run_Function_Response
	is := pb_istream_from_buffer(raw_data(input), c.size_t(len(input)))
	if !pb_decode(&is, &Run_Function_Request_msg, &req) {
		reason := string(is.errmsg) if is.errmsg != nil else "(none)"
		fatal_response(&rsp, "", strings.concatenate({"cannot decode RunFunctionRequest: ", reason}))
		return encode(&rsp)
	}
	tag := string(req.meta.tag) if req.has_meta && req.meta.tag != nil else ""
	set_meta(&rsp, tag)
	if msg := run_function(&req, &rsp); msg != "" {
		fatal_response(&rsp, tag, msg)
	}
	return encode(&rsp)
}

// encode writes rsp into a buffer on the context's heap, sized by the codec,
// that the canonical ABI owns from the moment it is returned.
encode :: proc(rsp: ^Run_Function_Response) -> (out: []u8, ok: bool) {
	size: c.size_t
	if !pb_get_encoded_size(&size, &Run_Function_Response_msg, rsp) {
		return nil, false
	}
	buf := make([]u8, max(int(size), 1))
	os := pb_ostream_from_buffer(raw_data(buf), size)
	if !pb_encode(&os, &Run_Function_Response_msg, rsp) {
		return nil, false
	}
	return buf[:os.bytes_written], true
}

set_meta :: proc(rsp: ^Run_Function_Response, tag: string) {
	rsp.has_meta = true
	rsp.meta.tag = strings.clone_to_cstring(tag)
	rsp.meta.has_ttl = true
	rsp.meta.ttl = {DEFAULT_TTL_SECONDS, 0}
}

// fatal_response makes rsp a fresh response carrying one fatal result.
fatal_response :: proc(rsp: ^Run_Function_Response, tag, msg: string) {
	rsp^ = {}
	set_meta(rsp, tag)
	result := new(Result)
	result^ = {
		severity   = .FATAL,
		message    = strings.clone_to_cstring(msg),
		has_target = true,
		target     = .COMPOSITE,
	}
	rsp.results = ([^]Result)(result)
	rsp.results_count = 1
}

// ─── log: the world's typed import ──────────────────────────────────────────

// Logging through the host's logger (the world's log import): a message and
// key/value pairs, typed, no payload encoding. debug lines show only when the
// runtime runs under --debug.
Log_Level :: enum u8 {
	Debug = 0,
	Info  = 1,
	Warn  = 2,
	Error = 3,
}

Log_Pair :: struct {
	key, value: string,
}

log_debug :: proc(msg: string, kv: ..Log_Pair) {
	log_at(.Debug, msg, kv)
}

log_info :: proc(msg: string, kv: ..Log_Pair) {
	log_at(.Info, msg, kv)
}

log_warn :: proc(msg: string, kv: ..Log_Pair) {
	log_at(.Warn, msg, kv)
}

log_error :: proc(msg: string, kv: ..Log_Pair) {
	log_at(.Error, msg, kv)
}

log_at :: proc(level: Log_Level, msg: string, kv: []Log_Pair) {
	when IS_WASM {
		items := make([]Bindings_Tuple2_String_String, len(kv))
		for p, i in kv {
			items[i] = {bindings_string(p.key), bindings_string(p.value)}
		}
		m := bindings_string(msg)
		list := Bindings_List_Tuple2_String_String{raw_data(items), c.size_t(len(items))}
		function_log(u8(level), &m, &list)
	} else {
		// Natively the line goes to stderr, as the other flavours' native
		// builds print it: built whole first, since the test runner's
		// threads would interleave the pieces.
		names := [Log_Level]string {
			.Debug = "debug",
			.Info  = "info",
			.Warn  = "warn",
			.Error = "error",
		}
		line := strings.concatenate({"wasmfn log ", names[level], ": ", msg})
		for p in kv {
			line = strings.concatenate({line, " ", p.key, "=", p.value})
		}
		runtime.print_string(strings.concatenate({line, "\n"}))
	}
}

// bindings_string views an Odin string as the bindings' string (the
// canonical ABI never writes through a string it is handed, the pointer is
// mutable only in the C type).
bindings_string :: proc(s: string) -> Bindings_String {
	return {raw_data(s), c.size_t(len(s))}
}

// ─── HTTP through the host ──────────────────────────────────────────────────

when !IS_WASM {
	// The host a native build talks to: none (every request fails with "no
	// host HTTP in this build") unless a test installs a procedure with
	// http_get_text's contract.
	test_host: proc(url: string) -> (text: string, err: string)
}

// http_get_text GETs url through the host (the c glue's wasi:http client)
// and returns the trimmed body of a 200, or the host's reason: the refusal
// of the egress grant or policy, a budget, a transport failure - never a
// trap.
http_get_text :: proc(url: string) -> (text: string, err: string) {
	when IS_WASM {
		reason: cstring
		body := wasmfn_http_get_text(strings.clone_to_cstring(url), &reason)
		if body == nil {
			return "", string(reason) if reason != nil else "unknown error"
		}
		return string(body), ""
	} else {
		if test_host == nil {
			return "", "no host HTTP in this build"
		}
		return test_host(url)
	}
}

// ─── the libc heap as an Odin allocator ─────────────────────────────────────

// dlmalloc's guaranteed alignment on wasm32; anything stricter goes through
// aligned_alloc.
MALLOC_ALIGNMENT :: 8

libc_allocator :: proc "contextless" () -> runtime.Allocator {
	return {procedure = libc_allocator_proc, data = nil}
}

libc_allocator_proc :: proc(
	_: rawptr,
	mode: runtime.Allocator_Mode,
	size, alignment: int,
	old_memory: rawptr,
	old_size: int,
	loc := #caller_location,
) -> (
	data: []byte,
	err: runtime.Allocator_Error,
) {
	switch mode {
	case .Alloc, .Alloc_Non_Zeroed:
		p := libc_alloc(size, alignment)
		if p == nil {
			return nil, .Out_Of_Memory
		}
		if mode == .Alloc {
			runtime.mem_zero(p, size)
		}
		return ([^]byte)(p)[:size], nil
	case .Free:
		free(old_memory)
		return nil, nil
	case .Free_All:
		return nil, .Mode_Not_Implemented
	case .Resize, .Resize_Non_Zeroed:
		p: rawptr
		if alignment <= MALLOC_ALIGNMENT {
			p = realloc(old_memory, c.size_t(size))
		} else {
			p = libc_alloc(size, alignment)
			if p != nil && old_memory != nil {
				runtime.mem_copy_non_overlapping(p, old_memory, min(size, old_size))
				free(old_memory)
			}
		}
		if p == nil {
			return nil, .Out_Of_Memory
		}
		if mode == .Resize && size > old_size {
			runtime.mem_zero(([^]byte)(p)[old_size:], size - old_size)
		}
		return ([^]byte)(p)[:size], nil
	case .Query_Features:
		set := (^runtime.Allocator_Mode_Set)(old_memory)
		if set != nil {
			set^ = {.Alloc, .Alloc_Non_Zeroed, .Free, .Resize, .Resize_Non_Zeroed, .Query_Features}
		}
		return nil, nil
	case .Query_Info:
		return nil, .Mode_Not_Implemented
	}
	return nil, nil
}

libc_alloc :: proc(size, alignment: int) -> rawptr {
	if alignment <= MALLOC_ALIGNMENT {
		return malloc(c.size_t(size))
	}
	// aligned_alloc wants a size that is a multiple of the alignment.
	rounded := (size + alignment - 1) / alignment * alignment
	return aligned_alloc(c.size_t(alignment), c.size_t(rounded))
}
