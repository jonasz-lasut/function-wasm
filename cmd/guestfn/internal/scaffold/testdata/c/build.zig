const std = @import("std");

// The guest's own sources; fn.c is the function, the rest is the ABI glue.
const guest_sources = [_][]const u8{ "src/fn.c", "src/structpb.c", "src/wasmfn.c" };
// The codec nanopb_generator wrote from proto/ (zig build gen-proto).
const codec_sources = [_][]const u8{
    "src/fnv1/run_function.pb.c",
    "src/fnv1/google/protobuf/struct.pb.c",
    "src/fnv1/google/protobuf/duration.pb.c",
};
// Every C file sees the same nanopb configuration: heap-allocated dynamic
// fields and 32-bit sizes, so a string, a map or a bytes field is never
// bounded by the codec. The guest's own sources are held to -Werror; gnu11
// rather than c11 because glibc hides POSIX's strdup under strict ISO C.
const pb_flags = [_][]const u8{ "-DPB_ENABLE_MALLOC=1", "-DPB_FIELD_32BIT=1" };
const guest_flags = pb_flags ++ [_][]const u8{ "-std=gnu11", "-Wall", "-Wextra", "-Werror" };

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const native = b.standardTargetOptions(.{});
    const wasm = b.resolveTargetQuery(.{ .cpu_arch = .wasm32, .os_tag = .wasi });

    // The wasip1 reactor the runtime loads.
    const exe = b.addExecutable(.{ .name = "fn", .root_module = guestModule(b, wasm, optimize, &.{}) });
    exe.entry = .disabled;
    exe.rdynamic = true;
    exe.wasi_exec_model = .reactor;
    b.installArtifact(exe);

    // Native unit tests: fn_test.c is their main.
    const tests = b.addExecutable(.{ .name = "fn_test", .root_module = guestModule(b, native, optimize, &.{"src/fn_test.c"}) });
    b.step("test", "Run unit tests").dependOn(&b.addRunArtifact(tests).step);

    // Regenerate the fnv1 codec from the vendored proto: zig build gen-proto
    // (needs nanopb_generator on PATH: pip install nanopb==0.4.9.1).
    const gen = b.step("gen-proto", "generate the fnv1 codec from proto/ with nanopb_generator");
    const generator = b.addSystemCommand(&.{
        "nanopb_generator",
        "-I",
        "proto",
        "-D",
        "src/fnv1",
        "proto/run_function.proto",
        "proto/google/protobuf/struct.proto",
        "proto/google/protobuf/duration.proto",
    });
    generator.setCwd(b.path(""));
    gen.dependOn(&generator.step);
}

// guestModule is the guest compiled for target: its sources, the generated
// codec, nanopb and cJSON from the build.zig.zon dependencies, and libc (the
// wasm build links zig's wasi-libc for malloc and string.h).
fn guestModule(b: *std.Build, target: std.Build.ResolvedTarget, optimize: std.builtin.OptimizeMode, extra: []const []const u8) *std.Build.Module {
    const nanopb = b.dependency("nanopb", .{});
    const cjson = b.dependency("cjson", .{});
    const mod = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true });
    mod.addIncludePath(b.path("src"));
    mod.addIncludePath(b.path("src/fnv1"));
    mod.addIncludePath(nanopb.path(""));
    mod.addIncludePath(cjson.path(""));
    mod.addCSourceFiles(.{ .root = nanopb.path(""), .files = &.{ "pb_common.c", "pb_decode.c", "pb_encode.c" }, .flags = &pb_flags });
    mod.addCSourceFiles(.{ .root = cjson.path(""), .files = &.{"cJSON.c"}, .flags = &pb_flags });
    mod.addCSourceFiles(.{ .files = &codec_sources, .flags = &pb_flags });
    mod.addCSourceFiles(.{ .files = &guest_sources, .flags = &guest_flags });
    mod.addCSourceFiles(.{ .files = extra, .flags = &guest_flags });
    return mod;
}
