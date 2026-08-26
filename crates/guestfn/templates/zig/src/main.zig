//! The hello-zig guest: a Crossplane composition function compiled to a wasip1
//! reactor and run by function-wasm. It composes a ConfigMap greeting the
//! composite resource in a ~35 KB module.
//!
//! `runFunction` is ordinary Zig over the protobuf messages zig-protobuf
//! generated from the vendored crossplane proto (src/fnv1); the `wasmfn_alloc`
//! and `wasmfn_run` exports and the `wasmfn.log` / `wasmfn.http` host imports
//! are the function-wasm ABI. Only the exports and the host imports are
//! wasi-specific, so the logic also builds and tests natively (`zig build test`).

const std = @import("std");
const builtin = @import("builtin");
const pb = @import("protobuf");
const v1 = @import("fnv1/apiextensions/fn/proto/v1.pb.zig");

const Struct = pb.wkt.Struct;
const Value = pb.wkt.Value;
const Duration = pb.wkt.Duration;

const is_wasi = builtin.target.os.tag == .wasi;
const default_ttl_seconds: i64 = 60;

// A fresh wasm instance serves each request (the host drops the store), so a
// bump allocator over a static heap needs no reset and no free.
var heap: [32 << 20]u8 = undefined;
var fba = std.heap.FixedBufferAllocator.init(&heap);
fn alloc() std.mem.Allocator {
    return fba.allocator();
}

// ─── ABI v1 exports ─────────────────────────────────────────────────────────

export fn wasmfn_alloc(size: u32) u32 {
    const buf = alloc().alloc(u8, size) catch return 0;
    return @intCast(@intFromPtr(buf.ptr));
}

export fn wasmfn_run(ptr: u32, len: u32) u64 {
    const input = @as([*]const u8, @ptrFromInt(@as(usize, ptr)))[0..len];
    const out = handle(input);
    return (@as(u64, @intCast(@intFromPtr(out.ptr))) << 32) | @as(u64, out.len);
}

/// The guest half of ABI v1: decode, run, encode. Every failure becomes a
/// fatal result so the host can always decode the reply.
fn handle(input: []const u8) []const u8 {
    const a = alloc();
    var reader = std.Io.Reader.fixed(input);
    var req = v1.RunFunctionRequest.decode(&reader, a) catch {
        return encode(fatal(a, "", "cannot decode RunFunctionRequest"));
    };
    const tag = if (req.meta) |m| m.tag else "";
    return switch (runFunction(a, &req)) {
        .ok => |rsp| encode(rsp),
        .err => |msg| encode(fatal(a, tag, msg)),
    };
}

fn encode(rsp: v1.RunFunctionResponse) []const u8 {
    var w: std.Io.Writer.Allocating = .init(alloc());
    var r = rsp;
    r.encode(&w.writer, alloc()) catch return &.{};
    return w.written();
}

// ─── the function ───────────────────────────────────────────────────────────

const Outcome = union(enum) { ok: v1.RunFunctionResponse, err: []const u8 };

/// Adds a ConfigMap greeting the composite resource to the desired state.
fn runFunction(a: std.mem.Allocator, req: *v1.RunFunctionRequest) Outcome {
    logger.info(a, "Running function", &.{.{ "tag", if (req.meta) |m| m.tag else "" }});

    var greeting: []const u8 = switch (configString(req, "greeting")) {
        .absent => "hello",
        .value => |g| g,
        .not_string => return .{ .err = "cannot read config: greeting must be a string" },
    };
    // greetingUrl fetches the greeting through the host instead - the
    // requires.egress grant of the module's manifest decides whether it may.
    switch (configString(req, "greetingUrl")) {
        .absent => {},
        .not_string => return .{ .err = "cannot read config: greetingUrl must be a string" },
        .value => |url| greeting = egress.getText(a, url) catch |e| return .{
            .err = std.fmt.allocPrint(a, "cannot fetch greeting: {s}", .{egress.reason(e)}) catch "cannot fetch greeting",
        },
    }

    const name = observedName(req) orelse
        return .{ .err = "cannot get observed composite resource: none in request" };

    const data = object(a, &.{.{ "greeting", string(std.fmt.allocPrint(a, "{s} {s}", .{ greeting, name }) catch return oom) }}) catch return oom;
    const cm = object(a, &.{
        .{ "apiVersion", string("v1") },
        .{ "kind", string("ConfigMap") },
        .{ "data", structVal(data) },
    }) catch return oom;

    var desired = if (req.desired) |d| d else v1.State{};
    desired.resources.append(a, .{ .key = "greeting", .value = .{ .resource = cm } }) catch return oom;

    var results: std.ArrayList(v1.Result) = .empty;
    results.append(a, .{
        .severity = .SEVERITY_NORMAL,
        .message = std.fmt.allocPrint(a, "greeted {s}", .{name}) catch return oom,
        .target = .TARGET_COMPOSITE,
    }) catch return oom;
    var conditions: std.ArrayList(v1.Condition) = .empty;
    conditions.append(a, .{
        .type = "FunctionSuccess",
        .status = .STATUS_CONDITION_TRUE,
        .reason = "Success",
        .target = .TARGET_COMPOSITE_AND_CLAIM,
    }) catch return oom;

    return .{ .ok = .{
        .meta = .{ .tag = if (req.meta) |m| m.tag else "", .ttl = .{ .seconds = default_ttl_seconds, .nanos = 0 } },
        .desired = desired,
        .results = results,
        .conditions = conditions,
    } };
}

const oom = Outcome{ .err = "out of memory" };

fn fatal(a: std.mem.Allocator, tag: []const u8, msg: []const u8) v1.RunFunctionResponse {
    var results: std.ArrayList(v1.Result) = .empty;
    results.append(a, .{ .severity = .SEVERITY_FATAL, .message = msg, .target = .TARGET_COMPOSITE }) catch {};
    return .{
        .meta = .{ .tag = tag, .ttl = .{ .seconds = default_ttl_seconds, .nanos = 0 } },
        .results = results,
    };
}

// ─── structpb helpers ───────────────────────────────────────────────────────

const CfgResult = union(enum) { absent, value: []const u8, not_string };

/// Reads a string field of the Input's `config` block.
fn configString(req: *v1.RunFunctionRequest, key: []const u8) CfgResult {
    const input = req.input orelse return .absent;
    const cfg = structValue(field(input, "config") orelse return .absent) orelse return .absent;
    const v = field(cfg, key) orelse return .absent;
    return if (stringValue(v)) |s| .{ .value = s } else .not_string;
}

fn observedName(req: *v1.RunFunctionRequest) ?[]const u8 {
    const res = (((req.observed orelse return null).composite) orelse return null).resource orelse return null;
    const md = structValue(field(res, "metadata") orelse return null) orelse return null;
    return stringValue(field(md, "name") orelse return null);
}

fn field(s: Struct, key: []const u8) ?Value {
    for (s.fields.items) |e| {
        if (std.mem.eql(u8, e.key, key)) return e.value;
    }
    return null;
}

fn stringValue(v: Value) ?[]const u8 {
    return if (v.kind) |k| switch (k) {
        .string_value => |s| s,
        else => null,
    } else null;
}

fn structValue(v: Value) ?Struct {
    return if (v.kind) |k| switch (k) {
        .struct_value => |s| s,
        else => null,
    } else null;
}

fn string(s: []const u8) Value {
    return .{ .kind = .{ .string_value = s } };
}

fn structVal(s: Struct) Value {
    return .{ .kind = .{ .struct_value = s } };
}

const Entry = struct { []const u8, Value };

fn object(a: std.mem.Allocator, entries: []const Entry) !Struct {
    var s = Struct{};
    for (entries) |e| try s.fields.append(a, .{ .key = e[0], .value = e[1] });
    return s;
}

// ─── wasmfn.log ─────────────────────────────────────────────────────────────

const logger = struct {
    fn info(a: std.mem.Allocator, msg: []const u8, kv: []const struct { []const u8, []const u8 }) void {
        var w: std.Io.Writer.Allocating = .init(a);
        const b = &w.writer;
        b.writeAll("{\"msg\":") catch return;
        jsonString(b, msg) catch return;
        b.writeAll(",\"kv\":[") catch return;
        for (kv, 0..) |pair, i| {
            if (i > 0) b.writeByte(',') catch return;
            jsonString(b, pair[0]) catch return;
            b.writeByte(',') catch return;
            jsonString(b, pair[1]) catch return;
        }
        b.writeAll("]}") catch return;
        emit(0, w.written());
    }

    fn jsonString(b: *std.Io.Writer, s: []const u8) !void {
        try b.writeByte('"');
        for (s) |c| switch (c) {
            '"' => try b.writeAll("\\\""),
            '\\' => try b.writeAll("\\\\"),
            '\n' => try b.writeAll("\\n"),
            '\r' => try b.writeAll("\\r"),
            '\t' => try b.writeAll("\\t"),
            else => if (c < 0x20) try b.print("\\u{x:0>4}", .{c}) else try b.writeByte(c),
        };
        try b.writeByte('"');
    }

    fn emit(level: u32, payload: []const u8) void {
        if (is_wasi) {
            log(level, @intCast(@intFromPtr(payload.ptr)), @intCast(payload.len));
        } else {
            std.debug.print("wasmfn log {s}\n", .{payload});
        }
    }
};

extern "wasmfn" fn log(level: u32, ptr: u32, len: u32) void;

// ─── wasmfn.http ────────────────────────────────────────────────────────────

const egress = struct {
    const Error = error{ Refused, BadResponse, NoHost };

    /// GETs url through the host and returns the trimmed body of a 200.
    fn getText(a: std.mem.Allocator, url: []const u8) ![]const u8 {
        var w: std.Io.Writer.Allocating = .init(a);
        try w.writer.print("{{\"method\":\"GET\",\"url\":", .{});
        try logger.jsonString(&w.writer, url);
        try w.writer.writeByte('}');
        const raw = try call(a, w.written());
        const parsed = std.json.parseFromSlice(HostResponse, a, raw, .{ .ignore_unknown_fields = true }) catch return Error.BadResponse;
        const r = parsed.value;
        if (r.status == 0) {
            last_reason = r.@"error" orelse "the host returned no status and no error";
            return Error.Refused;
        }
        const body = if (r.body) |b64| decodeBase64(a, b64) catch return Error.BadResponse else "";
        if (r.status != 200) {
            last_reason = std.fmt.allocPrint(a, "GET {s}: status {d}", .{ url, r.status }) catch "bad status";
            return Error.Refused;
        }
        return std.mem.trim(u8, body, " \t\r\n");
    }

    const HostResponse = struct { status: i64 = 0, body: ?[]const u8 = null, @"error": ?[]const u8 = null };

    var last_reason: []const u8 = "";
    fn reason(e: anyerror) []const u8 {
        return switch (e) {
            Error.Refused => last_reason,
            Error.NoHost => "no host HTTP in this build",
            else => "the host's HTTP response could not be decoded",
        };
    }

    fn call(a: std.mem.Allocator, payload: []const u8) ![]const u8 {
        if (!is_wasi) return if (test_host) |h| h(a, payload) else Error.NoHost;
        const packed_ptr = http(@intCast(@intFromPtr(payload.ptr)), @intCast(payload.len));
        if (packed_ptr == 0) return Error.BadResponse;
        const p: u32 = @intCast(packed_ptr >> 32);
        const n: u32 = @intCast(packed_ptr & 0xffffffff);
        return @as([*]const u8, @ptrFromInt(@as(usize, p)))[0..n];
    }

    fn decodeBase64(a: std.mem.Allocator, s: []const u8) ![]const u8 {
        const dec = std.base64.standard.Decoder;
        const n = try dec.calcSizeForSlice(s);
        const out = try a.alloc(u8, n);
        try dec.decode(out, s);
        return out;
    }

    // A native test may install a fake host.
    var test_host: ?*const fn (std.mem.Allocator, []const u8) anyerror![]const u8 = null;
};

extern "wasmfn" fn http(ptr: u32, len: u32) u64;

// ─── tests ──────────────────────────────────────────────────────────────────

// The guest bump-allocates and never frees (a fresh wasm instance per request
// drops it); the native tests run each case in an arena so the test allocator
// sees no leak.
test "default greeting" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    var req = v1.RunFunctionRequest{ .meta = .{ .tag = "hello" }, .observed = xr(a, "my-xr") };
    const rsp = runFunction(a, &req).ok;
    try std.testing.expectEqualStrings("hello my-xr", greetingOf(rsp));
    try std.testing.expectEqualStrings("hello", rsp.meta.?.tag);
    try std.testing.expectEqualStrings("greeted my-xr", rsp.results.items[0].message);
    try std.testing.expectEqualStrings("FunctionSuccess", rsp.conditions.items[0].type);
}

test "configured greeting keeps desired" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    var desired = v1.State{};
    try desired.resources.append(a, .{ .key = "other", .value = .{} });
    var req = v1.RunFunctionRequest{
        .meta = .{ .tag = "hello" },
        .input = try object(a, &.{.{ "config", structVal(try object(a, &.{.{ "greeting", string("hi") }})) }}),
        .observed = xr(a, "my-xr"),
        .desired = desired,
    };
    const rsp = runFunction(a, &req).ok;
    try std.testing.expectEqualStrings("hi my-xr", greetingOf(rsp));
    try std.testing.expectEqual(@as(usize, 2), rsp.desired.?.resources.items.len);
}

test "bad config is an error" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    var req = v1.RunFunctionRequest{
        .input = try object(a, &.{.{ "config", structVal(try object(a, &.{.{ "greeting", Value{ .kind = .{ .number_value = 7 } } }})) }}),
        .observed = xr(a, "my-xr"),
    };
    try std.testing.expectEqualStrings("cannot read config: greeting must be a string", runFunction(a, &req).err);
}

test "greeting from url through the host" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    egress.test_host = struct {
        fn h(_: std.mem.Allocator, payload: []const u8) anyerror![]const u8 {
            if (std.mem.indexOf(u8, payload, "https://greetings.example.com/en") != null) {
                return "{\"status\":200,\"body\":\"aG93ZHkK\"}"; // "howdy\n"
            }
            return "{\"status\":0,\"error\":\"sandbox.egress: no rule admits host \\\"evil.example.com\\\"\"}";
        }
    }.h;
    defer egress.test_host = null;
    var ok = v1.RunFunctionRequest{
        .meta = .{ .tag = "hello" },
        .input = try object(a, &.{.{ "config", structVal(try object(a, &.{.{ "greetingUrl", string("https://greetings.example.com/en") }})) }}),
        .observed = xr(a, "my-xr"),
    };
    try std.testing.expectEqualStrings("howdy my-xr", greetingOf(runFunction(a, &ok).ok));
    var bad = v1.RunFunctionRequest{
        .input = try object(a, &.{.{ "config", structVal(try object(a, &.{.{ "greetingUrl", string("https://evil.example.com/en") }})) }}),
        .observed = xr(a, "my-xr"),
    };
    try std.testing.expectEqualStrings(
        "cannot fetch greeting: sandbox.egress: no rule admits host \"evil.example.com\"",
        runFunction(a, &bad).err,
    );
}

fn xr(a: std.mem.Allocator, name: []const u8) v1.State {
    const res = object(a, &.{
        .{ "apiVersion", string("example.org/v1") },
        .{ "kind", string("XR") },
        .{ "metadata", structVal(object(a, &.{.{ "name", string(name) }}) catch unreachable) },
    }) catch unreachable;
    return .{ .composite = .{ .resource = res } };
}

fn greetingOf(rsp: v1.RunFunctionResponse) []const u8 {
    const cm = rsp.desired.?.resources.items[rsp.desired.?.resources.items.len - 1].value.?.resource.?;
    const data = structValue(field(cm, "data").?).?;
    return stringValue(field(data, "greeting").?).?;
}
