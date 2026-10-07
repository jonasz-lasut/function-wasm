const std = @import("std");

// The Odin half of the guest: one package, compiled by odin to a wasm32
// object the core module links. Every .odin file is an input of that step so
// the object is rebuilt when any of them changes.
const odin_sources = [_][]const u8{ "src/main.odin", "src/wasmfn.odin", "src/fnv1.odin", "src/structpb.odin" };
// The C half: the wasi:http client of the c flavour's glue (src/wasmfn.c),
// the codec nanopb_generator wrote from proto/ (src/fnv1, the c flavour's
// byte for byte) and the bindings wit-bindgen c wrote from wit/ (src/gen,
// the c flavour's byte for byte): the run export shim, the log import, the
// wasi:http client interfaces and cabi_realloc.
const glue_sources = [_][]const u8{"src/wasmfn.c"};
const codec_sources = [_][]const u8{
    "src/fnv1/run_function.pb.c",
    "src/fnv1/google/protobuf/struct.pb.c",
    "src/fnv1/google/protobuf/duration.pb.c",
};
const binding_sources = [_][]const u8{"src/gen/function.c"};
const nanopb_sources = [_][]const u8{ "pb_common.c", "pb_decode.c", "pb_encode.c" };
// Every C file sees the same nanopb configuration as the c flavour:
// heap-allocated dynamic fields and 32-bit sizes - the layout src/fnv1.odin
// mirrors. The glue is held to -Werror; gnu11 for strdup.
const pb_flags = [_][]const u8{ "-DPB_ENABLE_MALLOC=1", "-DPB_FIELD_32BIT=1" };
const glue_flags = pb_flags ++ [_][]const u8{ "-std=gnu11", "-Wall", "-Wextra", "-Werror" };

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const native = b.standardTargetOptions(.{});
    const nanopb = b.dependency("nanopb", .{});
    const includes = [_]std.Build.LazyPath{ b.path("src"), b.path("src/fnv1"), b.path("src/gen"), nanopb.path("") };

    // odin build src: the Odin package as one freestanding wasm32 object.
    // Freestanding, because the Odin runtime then imports nothing (no
    // wasi_snapshot_preview1, so guestfn build links no adapter); the libc
    // its allocations come from is the wasi-libc linked below.
    // -no-entry-point: the world's run export is the entry.
    const odin = b.addSystemCommand(&.{ "odin", "build", "src", "-target:freestanding_wasm32", "-build-mode:obj", "-no-entry-point", odinOptimize(optimize) });
    odin.setCwd(b.path(""));
    for (odin_sources) |src| odin.addFileInput(b.path(src));
    const guest_object = odin.addPrefixedOutputFileArg("-out:", "guest.o");

    // The C half, one object per file, through zig cc (clang).
    var objects: std.ArrayList(std.Build.LazyPath) = .empty;
    for (nanopb_sources) |src| objects.append(b.allocator, compileC(b, optimize, nanopb.path(src), &pb_flags, &includes)) catch @panic("OOM");
    for (codec_sources) |src| objects.append(b.allocator, compileC(b, optimize, b.path(src), &pb_flags, &includes)) catch @panic("OOM");
    for (glue_sources) |src| objects.append(b.allocator, compileC(b, optimize, b.path(src), &glue_flags, &includes)) catch @panic("OOM");
    for (binding_sources) |src| objects.append(b.allocator, compileC(b, optimize, b.path(src), &pb_flags, &includes)) catch @panic("OOM");

    // The core module, zig-out/bin/fn.wasm: a wasip1 reactor whose exports
    // are the canonical ABI's (run, cabi_post_run, cabi_realloc) and which
    // carries the component-type custom section of the bindings' object file
    // - the guest's world, encoded. guestfn build wraps it into the component
    // the runtime loads; it imports nothing from wasi_snapshot_preview1 (the
    // wasi-libc it links needs nothing from the host for malloc and strings),
    // so no adapter is linked in. Linked by zig cc rather than zig's own
    // build-exe step: Odin's runtime defines bzero (strongly, on every wasm
    // target) with a return value where libc's has none, and wasm-ld words
    // that as a signature-mismatch warning that build-exe turns into a
    // failure; zig cc passes the warning through (Odin's definition wins over
    // zig's weak one, nothing calls bzero).
    const link = b.addSystemCommand(&.{ b.graph.zig_exe, "cc", "--target=wasm32-wasi", "-mexec-model=reactor", ccOptimize(optimize) });
    link.addFileArg(guest_object);
    for (objects.items) |object| link.addFileArg(object);
    link.addFileArg(b.path("src/gen/function_component_type.o"));
    link.addArg("-o");
    const core = link.addOutputFileArg("fn.wasm");
    b.getInstallStep().dependOn(&b.addInstallBinFile(core, "fn.wasm").step);

    // zig build test: the unit tests, natively. The C half the tests need
    // (nanopb and the codec; the bindings and the wasi:http client are
    // wasi-only) is compiled for the host into objects under
    // zig-out/lib/guestc, which src/fnv1.odin names for the Odin test build
    // to link (objects rather than an archive: zig's archiver pads mach-o
    // members in a way Apple's ld rejects), then odin test runs
    // src/fn_test.odin, vetted like the wasm build.
    const tests = b.addSystemCommand(&.{ "odin", "test", "src", "-vet", "-strict-style" });
    tests.setCwd(b.path(""));
    _ = tests.addPrefixedOutputFileArg("-out:", "fn_test");
    tests.has_side_effects = true;
    for (nanopb_sources) |src| tests.step.dependOn(nativeObject(b, native, optimize, nanopb.path(src), src, &includes));
    for (codec_sources) |src| tests.step.dependOn(nativeObject(b, native, optimize, b.path(src), src, &includes));
    b.step("test", "Run the unit tests natively").dependOn(&tests.step);

    // zig build lint: the Odin sources type-checked and vetted for the wasm
    // target (the layout #asserts in src/fnv1.odin run here too).
    const lint = b.step("lint", "Type-check and vet the Odin sources for the wasm target");
    const vet = b.addSystemCommand(&.{ "odin", "check", "src", "-target:freestanding_wasm32", "-no-entry-point", "-vet", "-strict-style" });
    vet.setCwd(b.path(""));
    lint.dependOn(&vet.step);

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

    // Regenerate the world's bindings: zig build gen-bindings (needs
    // wit-bindgen on PATH at the version the checked-in src/gen names in its
    // header, wit-bindgen-cli 0.62.0).
    const gen_bindings = b.step("gen-bindings", "generate src/gen from wit/ with wit-bindgen c");
    const bindgen = b.addSystemCommand(&.{ "wit-bindgen", "c", "wit", "--world", "function", "--out-dir", "src/gen" });
    bindgen.setCwd(b.path(""));
    gen_bindings.dependOn(&bindgen.step);
}

// compileC compiles one C file for wasm32-wasi into an object in the cache.
fn compileC(b: *std.Build, optimize: std.builtin.OptimizeMode, source: std.Build.LazyPath, flags: []const []const u8, includes: []const std.Build.LazyPath) std.Build.LazyPath {
    const cc = b.addSystemCommand(&.{ b.graph.zig_exe, "cc", "--target=wasm32-wasi", ccOptimize(optimize), "-c" });
    cc.addArgs(flags);
    for (includes) |dir| cc.addPrefixedDirectoryArg("-I", dir);
    cc.addFileArg(source);
    cc.addArg("-o");
    return cc.addOutputFileArg("out.o");
}

// nativeObject compiles one C file for the host and installs the object as
// zig-out/lib/guestc/<stem>.o, where src/fnv1.odin names it for the tests.
// Without UBSan: a Debug build of C under zig instruments it, and the Odin
// test build that links the objects carries no sanitizer runtime.
fn nativeObject(b: *std.Build, target: std.Build.ResolvedTarget, optimize: std.builtin.OptimizeMode, source: std.Build.LazyPath, path: []const u8, includes: []const std.Build.LazyPath) *std.Build.Step {
    const mod = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true });
    for (includes) |dir| mod.addIncludePath(dir);
    mod.addCSourceFile(.{ .file = source, .flags = &(pb_flags ++ [_][]const u8{"-fno-sanitize=undefined"}) });
    const stem = std.fs.path.stem(path);
    const object = b.addObject(.{ .name = stem, .root_module = mod });
    return &b.addInstallFileWithDir(object.getEmittedBin(), .lib, b.fmt("guestc/{s}.o", .{stem})).step;
}

// odinOptimize maps zig's optimize mode onto odin's -o flag.
fn odinOptimize(mode: std.builtin.OptimizeMode) []const u8 {
    return switch (mode) {
        .Debug => "-o:none",
        .ReleaseSafe, .ReleaseFast => "-o:speed",
        .ReleaseSmall => "-o:size",
    };
}

// ccOptimize maps zig's optimize mode onto clang's -O flag.
fn ccOptimize(mode: std.builtin.OptimizeMode) []const u8 {
    return switch (mode) {
        .Debug => "-O0",
        .ReleaseSafe, .ReleaseFast => "-O2",
        .ReleaseSmall => "-Oz",
    };
}
