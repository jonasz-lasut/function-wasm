const std = @import("std");
const protobuf = @import("protobuf");

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const native = b.standardTargetOptions(.{});
    const wasm = b.resolveTargetQuery(.{ .cpu_arch = .wasm32, .os_tag = .wasi });

    const protobuf_dep = b.dependency("protobuf", .{ .target = native, .optimize = optimize });

    // The wasip1 reactor the runtime loads.
    const wasm_mod = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = wasm,
        .optimize = optimize,
    });
    wasm_mod.addImport("protobuf", protobuf_dep.module("protobuf"));
    const exe = b.addExecutable(.{ .name = "fn", .root_module = wasm_mod });
    exe.entry = .disabled;
    exe.rdynamic = true;
    exe.wasi_exec_model = .reactor;
    b.installArtifact(exe);

    // Native unit tests.
    const test_mod = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = native,
        .optimize = optimize,
    });
    test_mod.addImport("protobuf", protobuf_dep.module("protobuf"));
    const tests = b.addTest(.{ .root_module = test_mod });
    b.step("test", "Run unit tests").dependOn(&b.addRunArtifact(tests).step);

    // Regenerate the fnv1 codec from the vendored proto: zig build gen-proto.
    const gen = b.step("gen-proto", "generate the fnv1 codec from proto/run_function.proto");
    const protoc_step = protobuf.RunProtocStep.create(protobuf_dep.builder, native, .{
        .destination_directory = b.path("src/fnv1"),
        .source_files = &.{b.path("proto/run_function.proto")},
        .include_directories = &.{b.path("proto")},
    });
    gen.dependOn(&protoc_step.step);
}
