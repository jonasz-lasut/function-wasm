//! The function-wasm ABI v2 glue for a Zig guest (docs/abi-v2.md): the
//! world's `run` export (decode the request, run `runFunction`, encode the
//! response; every failure becomes a fatal result), the typed `log` import
//! as a logger and wasi:http@0.2's outgoing-handler as an HTTP client. The
//! component's surface is wit-bindgen's C bindings for wit/world.wit
//! (src/gen, read through translate-c as the `bindings` module): this file
//! implements the one C function they expect, `exports_function_run`, and
//! serves the generated C's libc calls from the guest's own bump heap, so
//! the core module links no libc and imports no wasi_snapshot_preview1 -
//! guestfn build wraps it into the component with no adapter. Only the
//! export and the imports are wasi-specific: natively the logger prints to
//! stderr and a test may install a fake host for HTTP, so the function
//! builds and tests as ordinary Zig.

const std = @import("std");
const builtin = @import("builtin");
const root = @import("root");
const v1 = @import("fnv1/apiextensions/fn/proto/v1.pb.zig");
const c = @import("bindings");

pub const is_wasi = builtin.target.os.tag == .wasi;
const default_ttl_seconds: i64 = 60;

/// What runFunction (src/main.zig) returns: a response, or the message of a
/// fatal result. A response without meta gets the request's tag and a 60 s
/// TTL from the glue.
pub const Outcome = union(enum) { ok: v1.RunFunctionResponse, err: []const u8 };

// A fresh wasm instance serves each request (the host drops the store), so a
// bump allocator over a static heap needs no reset and no free.
var heap: [32 << 20]u8 = undefined;
var fba = std.heap.FixedBufferAllocator.init(&heap);
fn alloc() std.mem.Allocator {
    return fba.allocator();
}

// ─── the run export ─────────────────────────────────────────────────────────

comptime {
    if (is_wasi) {
        // The C half of the export (src/gen/function.c) lifts run's list<u8>
        // argument and lowers the result; it calls this function.
        @export(&run, .{ .name = "exports_function_run" });
        // The generated C's libc: wit-bindgen's weak cabi_realloc calls
        // realloc (the canonical ABI's allocator, used by the host to lower
        // the request and by the bindings to lift every list and string),
        // its post-return hook calls free, and its helpers memcpy (Zig's
        // compiler-rt) and strlen. Nothing is ever freed: the instance is
        // dropped after the request, which is what makes cabi_post_run safe
        // over a bump heap.
        @export(&realloc, .{ .name = "realloc" });
        @export(&free, .{ .name = "free" });
        @export(&abort, .{ .name = "abort" });
        @export(&strlen, .{ .name = "strlen" });
    }
}

// The world's run: the host lowered the request bytes into a buffer of ours
// (cabi_realloc), the encoded response goes back as an owned list. An error
// string here becomes the request's fatal result on the host's side; the
// glue only reaches it when it cannot even encode a reply.
fn run(request: *c.function_list_u8_t, ret: *c.function_list_u8_t, err: *c.function_string_t) callconv(.c) bool {
    const input = if (request.len > 0) request.ptr[0..request.len] else "";
    const out = handle(input) catch {
        err.* = cstr("cannot encode RunFunctionResponse");
        return false;
    };
    ret.* = .{ .ptr = @constCast(out.ptr), .len = out.len };
    return true;
}

// realloc without the old size: every block carries its size in a header
// one alignment unit wide, so a grown block copies the right amount.
const block_align = 16;

fn realloc(ptr: ?*anyopaque, size: usize) callconv(.c) ?*anyopaque {
    const block = alloc().alignedAlloc(u8, .@"16", size + block_align) catch return null;
    std.mem.writeInt(usize, block[0..@sizeOf(usize)], size, .little);
    const out = block[block_align..];
    if (ptr) |p| {
        const old: [*]u8 = @ptrFromInt(@intFromPtr(p) - block_align);
        const old_size = std.mem.readInt(usize, old[0..@sizeOf(usize)], .little);
        const n = @min(old_size, size);
        @memcpy(out[0..n], old[block_align..][0..n]);
    }
    return out.ptr;
}

fn free(_: ?*anyopaque) callconv(.c) void {}

fn abort() callconv(.c) noreturn {
    @trap();
}

fn strlen(s: [*:0]const u8) callconv(.c) usize {
    return std.mem.len(s);
}

/// The guest half of the contract: decode, run, encode. Every failure the
/// guest can describe becomes a fatal result so the host can always decode
/// the reply; only a response that cannot be encoded uses run's error.
fn handle(input: []const u8) ![]const u8 {
    const a = alloc();
    var reader = std.Io.Reader.fixed(input);
    var req = v1.RunFunctionRequest.decode(&reader, a) catch {
        return encode(fatal(a, "", "cannot decode RunFunctionRequest"));
    };
    const tag = if (req.meta) |m| m.tag else "";
    return switch (root.runFunction(a, &req)) {
        .ok => |rsp| encode(withMeta(rsp, tag)),
        .err => |msg| encode(fatal(a, tag, msg)),
    };
}

fn encode(rsp: v1.RunFunctionResponse) ![]const u8 {
    var w: std.Io.Writer.Allocating = .init(alloc());
    var r = rsp;
    try r.encode(&w.writer, alloc());
    return w.written();
}

fn meta(tag: []const u8) v1.ResponseMeta {
    return .{ .tag = tag, .ttl = .{ .seconds = default_ttl_seconds, .nanos = 0 } };
}

fn withMeta(rsp: v1.RunFunctionResponse, tag: []const u8) v1.RunFunctionResponse {
    var r = rsp;
    if (r.meta == null) r.meta = meta(tag);
    return r;
}

fn fatal(a: std.mem.Allocator, tag: []const u8, msg: []const u8) v1.RunFunctionResponse {
    var results: std.ArrayList(v1.Result) = .empty;
    results.append(a, .{ .severity = .SEVERITY_FATAL, .message = msg, .target = .TARGET_COMPOSITE }) catch {};
    return .{ .meta = meta(tag), .results = results };
}

/// A Zig slice as the bindings' string (the canonical ABI never writes
/// through a string it is handed, the pointer is mutable only in the C type).
fn cstr(s: []const u8) c.function_string_t {
    return .{ .ptr = @constCast(s.ptr), .len = s.len };
}

// ─── log: the world's typed import ──────────────────────────────────────────

/// Logging through the host's logger (the world's log import): a message
/// and key/value pairs, typed, no payload encoding. debug lines show only
/// when the runtime runs under --debug.
pub const log = struct {
    pub const Pair = struct { []const u8, []const u8 };
    const Level = enum(u8) { debug = 0, info = 1, warn = 2, err = 3 };

    pub fn debug(a: std.mem.Allocator, msg: []const u8, kv: []const Pair) void {
        emit(a, .debug, msg, kv);
    }

    pub fn info(a: std.mem.Allocator, msg: []const u8, kv: []const Pair) void {
        emit(a, .info, msg, kv);
    }

    pub fn warn(a: std.mem.Allocator, msg: []const u8, kv: []const Pair) void {
        emit(a, .warn, msg, kv);
    }

    pub fn err(a: std.mem.Allocator, msg: []const u8, kv: []const Pair) void {
        emit(a, .err, msg, kv);
    }

    fn emit(a: std.mem.Allocator, level: Level, msg: []const u8, kv: []const Pair) void {
        if (!is_wasi) {
            std.debug.print("wasmfn log {s}: {s}", .{ @tagName(level), msg });
            for (kv) |p| std.debug.print(" {s}={s}", .{ p[0], p[1] });
            std.debug.print("\n", .{});
            return;
        }
        const pairs = a.alloc(c.function_tuple2_string_string_t, kv.len) catch return;
        for (kv, pairs) |p, *out| out.* = .{ .f0 = cstr(p[0]), .f1 = cstr(p[1]) };
        var m = cstr(msg);
        var list: c.function_list_tuple2_string_string_t = .{ .ptr = pairs.ptr, .len = pairs.len };
        c.function_log(@intFromEnum(level), &m, &list);
    }
};

// ─── HTTP over wasi:http@0.2 ────────────────────────────────────────────────

/// HTTP through the host (wasi:http/outgoing-handler). The guest never opens
/// a socket: the host performs the request within the egress grant of the
/// module's manifest and the operator's policy, or refuses it - the refusal
/// reaches the guest as wasi:http's internal-error code carrying the
/// runtime's reason, never as a trap.
pub const http = struct {
    pub const Error = error{ Refused, BadResponse, NoHost };

    /// GETs url through the host and returns the trimmed body of a 200 (any
    /// other status is an error here).
    pub fn getText(a: std.mem.Allocator, url: []const u8) ![]const u8 {
        const body = if (is_wasi)
            try wasihttp.get(a, url)
        else if (test_host) |h|
            try h(a, url)
        else
            return Error.NoHost;
        return std.mem.trim(u8, body, " \t\r\n");
    }

    /// The words for an error getText returned: the host's reason for a
    /// refusal, or what went wrong reading its answer.
    pub fn reason(e: anyerror) []const u8 {
        return switch (e) {
            Error.Refused => last_reason,
            Error.NoHost => "no host HTTP in this build",
            else => "the host's HTTP response could not be read",
        };
    }

    /// Fails a request with the host's reason; a test_host answers a
    /// refusal the way the host does by returning it.
    pub fn refuse(why: []const u8) Error {
        last_reason = why;
        return Error.Refused;
    }

    var last_reason: []const u8 = "";

    /// The host a native build talks to: none (every request fails with
    /// "no host HTTP in this build") unless a test installs a function with
    /// getText's contract. The wasi build ignores it.
    pub var test_host: ?*const fn (std.mem.Allocator, []const u8) anyerror![]const u8 = null;
};

const wasihttp = struct {
    const Url = struct { https: bool, authority: []const u8, path: []const u8 };

    fn parse(url: []const u8) ?Url {
        const sep = std.mem.indexOf(u8, url, "://") orelse return null;
        const scheme = url[0..sep];
        const https = std.mem.eql(u8, scheme, "https");
        if (!https and !std.mem.eql(u8, scheme, "http")) return null;
        const rest = url[sep + 3 ..];
        const slash = std.mem.indexOfScalar(u8, rest, '/') orelse rest.len;
        return .{
            .https = https,
            .authority = rest[0..slash],
            .path = if (slash == rest.len) "/" else rest[slash..],
        };
    }

    /// One GET over wasi:http@0.2: build the outgoing-request, hand it to
    /// outgoing-handler, block on the response future, drain the body
    /// stream. The host's refusals (no grant, a blocked address, a budget)
    /// arrive as the internal-error code carrying the runtime's reason.
    fn get(a: std.mem.Allocator, url: []const u8) ![]const u8 {
        const u = parse(url) orelse return http.refuse("only http and https URLs work");

        const headers = c.wasi_http_types_constructor_fields();
        const req = c.wasi_http_types_constructor_outgoing_request(headers);
        const req_b: c.wasi_http_types_borrow_outgoing_request_t = .{ .__handle = req.__handle };
        var method: c.wasi_http_types_method_t = .{ .tag = c.WASI_HTTP_TYPES_METHOD_GET };
        if (!c.wasi_http_types_method_outgoing_request_set_method(req_b, &method)) return http.refuse("the host refused the request method");
        var scheme: c.wasi_http_types_scheme_t = .{ .tag = if (u.https) c.WASI_HTTP_TYPES_SCHEME_HTTPS else c.WASI_HTTP_TYPES_SCHEME_HTTP };
        if (!c.wasi_http_types_method_outgoing_request_set_scheme(req_b, &scheme)) return http.refuse("the host refused the URL");
        var authority = cstr(u.authority);
        if (!c.wasi_http_types_method_outgoing_request_set_authority(req_b, &authority)) return http.refuse("the host refused the URL");
        var path = cstr(u.path);
        if (!c.wasi_http_types_method_outgoing_request_set_path_with_query(req_b, &path)) return http.refuse("the host refused the URL");

        var future: c.wasi_http_types_own_future_incoming_response_t = undefined;
        var code: c.wasi_http_types_error_code_t = undefined;
        if (!c.wasi_http_outgoing_handler_handle(req, null, &future, &code)) return http.refuse(errorText(a, code));
        defer c.wasi_http_types_future_incoming_response_drop_own(future);
        const future_b: c.wasi_http_types_borrow_future_incoming_response_t = .{ .__handle = future.__handle };

        const pollable = c.wasi_http_types_method_future_incoming_response_subscribe(future_b);
        c.wasi_io_poll_method_pollable_block(.{ .__handle = pollable.__handle });
        c.wasi_io_poll_pollable_drop_own(pollable);

        var got: c.wasi_http_types_result_result_own_incoming_response_error_code_void_t = undefined;
        if (!c.wasi_http_types_method_future_incoming_response_get(future_b, &got)) return http.refuse("the host answered nothing");
        if (got.is_err) return http.refuse("the response was already taken");
        if (got.val.ok.is_err) return http.refuse(errorText(a, got.val.ok.val.err));
        const response = got.val.ok.val.ok;
        defer c.wasi_http_types_incoming_response_drop_own(response);
        const response_b: c.wasi_http_types_borrow_incoming_response_t = .{ .__handle = response.__handle };
        const status = c.wasi_http_types_method_incoming_response_status(response_b);

        var body: c.wasi_http_types_own_incoming_body_t = undefined;
        if (!c.wasi_http_types_method_incoming_response_consume(response_b, &body)) return http.refuse("the response body was already taken");
        defer c.wasi_http_types_incoming_body_drop_own(body);
        var stream: c.wasi_io_streams_own_input_stream_t = undefined;
        if (!c.wasi_http_types_method_incoming_body_stream(.{ .__handle = body.__handle }, &stream)) return http.refuse("the response body stream was already taken");
        defer c.wasi_io_streams_input_stream_drop_own(stream);
        const stream_b: c.wasi_io_streams_borrow_input_stream_t = .{ .__handle = stream.__handle };

        var out: std.ArrayList(u8) = .empty;
        while (true) {
            var chunk: c.function_list_u8_t = undefined;
            var serr: c.wasi_io_streams_stream_error_t = undefined;
            if (!c.wasi_io_streams_method_input_stream_blocking_read(stream_b, 64 << 10, &chunk, &serr)) {
                if (serr.tag == c.WASI_IO_STREAMS_STREAM_ERROR_CLOSED) break;
                return http.refuse("reading the response body failed");
            }
            if (chunk.len > 0) try out.appendSlice(a, chunk.ptr[0..chunk.len]);
        }

        if (status != 200) {
            return http.refuse(std.fmt.allocPrint(a, "GET {s}: status {d}", .{ url, status }) catch "unexpected status");
        }
        return out.items;
    }

    /// Words a wasi:http error-code: internal-error carries the host's own
    /// reason - the refusal of the egress grant or policy, a budget, a
    /// transport failure - verbatim.
    fn errorText(a: std.mem.Allocator, code: c.wasi_http_types_error_code_t) []const u8 {
        if (code.tag == c.WASI_HTTP_TYPES_ERROR_CODE_INTERNAL_ERROR and code.val.internal_error.is_some) {
            const s = code.val.internal_error.val;
            return s.ptr[0..s.len];
        }
        return std.fmt.allocPrint(a, "wasi:http error-code {d}", .{code.tag}) catch "wasi:http error";
    }
};

test "url parsing" {
    const u = wasihttp.parse("http://127.0.0.1:9480/en").?;
    try std.testing.expect(!u.https);
    try std.testing.expectEqualStrings("127.0.0.1:9480", u.authority);
    try std.testing.expectEqualStrings("/en", u.path);
    const bare = wasihttp.parse("https://greetings.example.com").?;
    try std.testing.expect(bare.https);
    try std.testing.expectEqualStrings("/", bare.path);
    try std.testing.expect(wasihttp.parse("ftp://x/y") == null);
}
