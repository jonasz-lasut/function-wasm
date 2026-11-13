const std = @import("std");

// The guest: fn.c is the C adapter over the generated protobuf structs,
// rotator.cpp and tls.cpp the function's logic in C++ over mbedTLS,
// entropy.c mbedTLS's randomness from getentropy, the rest is the c
// flavour's ABI glue.
const adapter_sources = [_][]const u8{ "src/fn.c", "src/structpb.c", "src/wasmfn.c", "src/entropy.c" };
const logic_sources = [_][]const u8{ "src/rotator.cpp", "src/tls.cpp" };
// The codec nanopb_generator wrote from proto/ (zig build gen-proto).
const codec_sources = [_][]const u8{
    "src/fnv1/run_function.pb.c",
    "src/fnv1/google/protobuf/struct.pb.c",
    "src/fnv1/google/protobuf/duration.pb.c",
};
// The bindings wit-bindgen c wrote from wit/ (zig build gen-bindings): the
// run export shim, the log import, the wasi:http client and cabi_realloc.
const binding_sources = [_][]const u8{"src/gen/function.c"};
// The mbedTLS sources the configuration leaves with code: the rest of the
// library compiles to nothing under it and is left out.
const mbedtls_sources = [_][]const u8{
    "library/aes.c",
    "library/asn1parse.c",
    "library/asn1write.c",
    "library/base64.c",
    "library/bignum.c",
    "library/bignum_core.c",
    "library/constant_time.c",
    "library/ctr_drbg.c",
    "library/ecdsa.c",
    "library/ecp.c",
    "library/ecp_curves.c",
    "library/entropy.c",
    "library/error.c",
    "library/hmac_drbg.c",
    "library/md.c",
    "library/oid.c",
    "library/pem.c",
    "library/pk.c",
    "library/pk_ecc.c",
    "library/pk_wrap.c",
    "library/pkparse.c",
    "library/pkwrite.c",
    "library/platform.c",
    "library/platform_util.c",
    "library/sha256.c",
    "library/x509.c",
    "library/x509_create.c",
    "library/x509_crt.c",
    "library/x509write.c",
    "library/x509write_crt.c",
};
// Every C file sees the same nanopb configuration: heap-allocated dynamic
// fields and 32-bit sizes, so a string, a map or a bytes field is never
// bounded by the codec. The guest's own sources are held to -Werror; gnu11
// rather than c11 because glibc hides POSIX's strdup under strict ISO C.
const pb_flags = [_][]const u8{ "-DPB_ENABLE_MALLOC=1", "-DPB_FIELD_32BIT=1" };
const adapter_flags = pb_flags ++ [_][]const u8{ "-std=gnu11", "-Wall", "-Wextra", "-Werror" };
// The C++ half: no exceptions and no RTTI, which wasm32-wasi has no
// unwinder for and the logic has no use for.
const logic_flags = [_][]const u8{ "-std=c++20", "-fno-exceptions", "-fno-rtti", "-Wall", "-Wextra", "-Werror" };
// mbedTLS reads its configuration from src/cert_rotator_mbedtls_config.h.
const mbedtls_flags = [_][]const u8{"-DMBEDTLS_CONFIG_FILE=\"cert_rotator_mbedtls_config.h\""};

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const native = b.standardTargetOptions(.{});
    const wasm = b.resolveTargetQuery(.{ .cpu_arch = .wasm32, .os_tag = .wasi });

    // The core module, zig-out/bin/fn.wasm: a wasip1 reactor whose exports
    // are the canonical ABI's (run, cabi_post_run, cabi_realloc) and which
    // carries the component-type custom section of the bindings' object file
    // - the guest's world, encoded. guestfn build wraps it into the component
    // the runtime loads, linking the wasip1 adapter in for what wasi-libc
    // imports here (the clock behind time() and the randomness behind
    // getentropy).
    const core = b.addExecutable(.{ .name = "fn", .root_module = guestModule(b, wasm, optimize, &.{}) });
    core.entry = .disabled;
    core.wasi_exec_model = .reactor;
    core.root_module.addIncludePath(b.path("src/gen"));
    core.root_module.addCSourceFiles(.{ .files = &binding_sources, .flags = &pb_flags });
    core.root_module.addObjectFile(b.path("src/gen/function_component_type.o"));
    b.installArtifact(core);

    // Native unit tests: fn_test.c drives run_function over the generated
    // structs, rotator_test.cpp the decision and the crypto. The bindings
    // are wasi-only (behind __wasi__ in wasmfn.c), so no generated code is
    // linked here.
    const test_step = b.step("test", "Run unit tests");
    const fn_test = b.addExecutable(.{ .name = "fn_test", .root_module = guestModule(b, native, optimize, &.{"src/fn_test.c"}) });
    test_step.dependOn(&b.addRunArtifact(fn_test).step);
    const rotator_test = b.addExecutable(.{ .name = "rotator_test", .root_module = guestModule(b, native, optimize, &.{"src/rotator_test.cpp"}) });
    test_step.dependOn(&b.addRunArtifact(rotator_test).step);

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
    // header, wit-bindgen-cli 0.63.0).
    const gen_bindings = b.step("gen-bindings", "generate src/gen from wit/ with wit-bindgen c");
    const bindgen = b.addSystemCommand(&.{ "wit-bindgen", "c", "wit", "--world", "function", "--out-dir", "src/gen" });
    bindgen.setCwd(b.path(""));
    gen_bindings.dependOn(&bindgen.step);
}

// guestModule is the guest compiled for target: its sources, the generated
// codec, nanopb and mbedTLS from the build.zig.zon dependencies, libc and
// libc++ (the wasm build links zig's wasi-libc and its libc++ for wasm).
// extra names the test main, C or C++.
fn guestModule(b: *std.Build, target: std.Build.ResolvedTarget, optimize: std.builtin.OptimizeMode, extra: []const []const u8) *std.Build.Module {
    const nanopb = b.dependency("nanopb", .{});
    const mbedtls = b.dependency("mbedtls", .{});
    const mod = b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true, .link_libcpp = true });
    mod.addIncludePath(b.path("src"));
    mod.addIncludePath(b.path("src/fnv1"));
    mod.addIncludePath(nanopb.path(""));
    mod.addIncludePath(mbedtls.path("include"));
    mod.addIncludePath(mbedtls.path("library"));
    mod.addCSourceFiles(.{ .root = nanopb.path(""), .files = &.{ "pb_common.c", "pb_decode.c", "pb_encode.c" }, .flags = &pb_flags });
    mod.addCSourceFiles(.{ .root = mbedtls.path(""), .files = &mbedtls_sources, .flags = &mbedtls_flags });
    mod.addCSourceFiles(.{ .files = &codec_sources, .flags = &pb_flags });
    mod.addCSourceFiles(.{ .files = &adapter_sources, .flags = &adapter_flags });
    mod.addCSourceFiles(.{ .files = &logic_sources, .flags = &(logic_flags ++ mbedtls_flags) });
    for (extra) |src| {
        if (std.mem.endsWith(u8, src, ".cpp")) {
            mod.addCSourceFile(.{ .file = b.path(src), .flags = &(logic_flags ++ mbedtls_flags) });
        } else {
            mod.addCSourceFile(.{ .file = b.path(src), .flags = &adapter_flags });
        }
    }
    return mod;
}
