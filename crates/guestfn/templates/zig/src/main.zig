//! The hello-zig guest: a Crossplane composition function in Zig, compiled
//! to the core module guestfn build wraps into an ABI v2 component, run by
//! function-wasm. It composes a ConfigMap greeting the composite resource.
//!
//! `runFunction` is ordinary Zig over the protobuf messages zig-protobuf
//! generated from the vendored crossplane proto (src/fnv1); the world's run
//! export and the log / wasi:http host imports live in wasmfn.zig, behind
//! the bindings wit-bindgen generated from wit/ (src/gen). Nothing here is
//! wasi-specific, so the logic also builds and tests natively (`zig build
//! test`).

const std = @import("std");
const pb = @import("protobuf");
const v1 = @import("fnv1/apiextensions/fn/proto/v1.pb.zig");
const wasmfn = @import("wasmfn.zig");

// The exports - the world's run, the bindings' libc - are declared in
// wasmfn.zig, which is only analysed (and its tests collected) once the root
// names it.
comptime {
    _ = wasmfn;
}

const Struct = pb.wkt.Struct;
const Value = pb.wkt.Value;

/// Adds a ConfigMap greeting the composite resource to the desired state.
pub fn runFunction(a: std.mem.Allocator, req: *v1.RunFunctionRequest) wasmfn.Outcome {
    wasmfn.log.info(a, "Running function", &.{.{ "tag", if (req.meta) |m| m.tag else "" }});

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
        .value => |url| greeting = wasmfn.http.getText(a, url) catch |e| return .{
            .err = std.fmt.allocPrint(a, "cannot fetch greeting: {s}", .{wasmfn.http.reason(e)}) catch "cannot fetch greeting",
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

    wasmfn.log.debug(a, "Composed the greeting", &.{ .{ "name", name }, .{ "greeting", greeting } });

    return .{ .ok = .{ .desired = desired, .results = results, .conditions = conditions } };
}

const oom = wasmfn.Outcome{ .err = "out of memory" };

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
    wasmfn.http.test_host = struct {
        fn h(_: std.mem.Allocator, url: []const u8) anyerror![]const u8 {
            if (std.mem.eql(u8, url, "https://greetings.example.com/en")) return "howdy\n";
            return wasmfn.http.refuse("sandbox.egress: no rule admits host \"evil.example.com\"");
        }
    }.h;
    defer wasmfn.http.test_host = null;
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
