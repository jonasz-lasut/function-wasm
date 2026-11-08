const std = @import("std");
const protobuf = @import("protobuf");

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const native = b.standardTargetOptions(.{});
    const wasm = b.resolveTargetQuery(.{ .cpu_arch = .wasm32, .os_tag = .wasi });

    const protobuf_dep = b.dependency("protobuf", .{ .target = native, .optimize = optimize });

    // The core module, zig-out/bin/fn.wasm: a wasip1 reactor whose exports
    // are the canonical ABI's (run, cabi_post_run, cabi_realloc) and which
    // carries the component-type custom section of the bindings' object file
    // - the guest's world, encoded. guestfn build wraps it into the component
    // the runtime loads. It links no libc: src/wasmfn.zig serves the
    // generated C's realloc, free, abort and strlen from the guest's own
    // heap (src/libc-shim declares them, memcpy is compiler-rt's), so the
    // module imports nothing from wasi_snapshot_preview1 and no adapter is
    // linked in.
    const core = b.addExecutable(.{ .name = "fn", .root_module = guestModule(b, wasm, optimize, protobuf_dep) });
    core.entry = .disabled;
    core.wasi_exec_model = .reactor;
    core.root_module.addIncludePath(b.path("src/libc-shim"));
    core.root_module.addCSourceFile(.{ .file = b.path("src/gen/function.c"), .flags = &.{ "-std=gnu11", "-fno-builtin" } });
    core.root_module.addObjectFile(b.path("src/gen/function_component_type.o"));
    b.installArtifact(core);

    // Native unit tests, over the same translated bindings.
    const tests = b.addTest(.{ .root_module = guestModule(b, native, optimize, protobuf_dep) });
    b.step("test", "Run unit tests").dependOn(&b.addRunArtifact(tests).step);

    // Regenerate the world's bindings: zig build gen-bindings (needs
    // wit-bindgen on PATH at the version the checked-in src/gen names in its
    // header, wit-bindgen-cli 0.62.0).
    const gen_bindings = b.step("gen-bindings", "generate src/gen from wit/ with wit-bindgen c");
    const bindgen = b.addSystemCommand(&.{ "wit-bindgen", "c", "wit", "--world", "function", "--out-dir", "src/gen" });
    bindgen.setCwd(b.path(""));
    gen_bindings.dependOn(&bindgen.step);

    // Regenerate the fnv1 codec from the vendored proto: zig build gen-proto.
    const gen = b.step("gen-proto", "generate the fnv1 codec from proto/run_function.proto");
    const protoc_step = protobuf.RunProtocStep.create(protobuf_dep.builder, native, .{
        .destination_directory = b.path("src/fnv1"),
        .source_files = &.{b.path("proto/run_function.proto")},
        .include_directories = &.{b.path("proto")},
    });
    gen.dependOn(&protoc_step.step);
}

// guestModule is src/main.zig compiled for target, with zig-protobuf and the
// wit-bindgen C bindings for wit/world.wit (src/gen/function.h, read through
// translate-c as the `bindings` module), without libc.
fn guestModule(b: *std.Build, target: std.Build.ResolvedTarget, optimize: std.builtin.OptimizeMode, protobuf_dep: *std.Build.Dependency) *std.Build.Module {
    const bindings = b.addTranslateC(.{
        .root_source_file = b.path("src/gen/function.h"),
        .target = target,
        .optimize = optimize,
        // The header needs only clang's own stdint/stdbool/stddef; with libc
        // on, Zig's libc layer would export realloc and free beside ours.
        .link_libc = false,
    });
    bindings.addIncludePath(b.path("src/gen"));
    const mod = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = false,
    });
    mod.addImport("protobuf", protobuf_dep.module("protobuf"));
    mod.addImport("bindings", bindings.createModule());
    return mod;
}
