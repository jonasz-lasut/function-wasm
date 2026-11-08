// The codec: nanopb through the FFI. The structs nanopb_generator wrote into
// src/fnv1 (run_function.pb.h, struct.pb.h, duration.pb.h, the c flavour's
// byte for byte, with its options: every dynamic field a heap pointer plus a
// 32-bit count, singular submessages inline behind a has_ flag) are declared
// again here with the same layout, so pb_decode fills them and pb_encode
// reads them through the C descriptors. Odin lays a struct out as C does
// (declaration order, natural alignment), and the #asserts at the end hold
// every size and offset the guest relies on to what zig cc computes for
// wasm32: a drift fails the build, never the run.
package guest

import "core:c"

pb_size_t :: u32

// ─── google.protobuf ────────────────────────────────────────────────────────

Null_Value :: c.int

Struct :: struct {
	fields_count: pb_size_t,
	fields:       [^]Struct_Fields_Entry,
}

List_Value :: struct {
	values_count: pb_size_t,
	values:       [^]Value,
}

Value :: struct {
	which_kind: pb_size_t,
	kind:       struct #raw_union {
		null_value:   Null_Value,
		number_value: f64,
		string_value: cstring,
		bool_value:   bool,
		struct_value: Struct,
		list_value:   List_Value,
	},
}

Struct_Fields_Entry :: struct {
	key:       cstring,
	has_value: bool,
	value:     Value,
}

// The oneof tags of Value.kind (google_protobuf_Value_*_tag).
VALUE_NULL_TAG :: 1
VALUE_NUMBER_TAG :: 2
VALUE_STRING_TAG :: 3
VALUE_BOOL_TAG :: 4
VALUE_STRUCT_TAG :: 5
VALUE_LIST_TAG :: 6

Duration :: struct {
	seconds: i64,
	nanos:   i32,
}

// ─── apiextensions.fn.proto.v1 ──────────────────────────────────────────────

Capability :: c.int

Ready :: enum c.int {
	UNSPECIFIED = 0,
	TRUE        = 1,
	FALSE       = 2,
}

Severity :: enum c.int {
	UNSPECIFIED = 0,
	FATAL       = 1,
	WARNING     = 2,
	NORMAL      = 3,
}

Target :: enum c.int {
	UNSPECIFIED         = 0,
	COMPOSITE           = 1,
	COMPOSITE_AND_CLAIM = 2,
}

Status :: enum c.int {
	UNSPECIFIED = 0,
	UNKNOWN     = 1,
	TRUE        = 2,
	FALSE       = 3,
}

Request_Meta :: struct {
	tag:                cstring,
	capabilities_count: pb_size_t,
	capabilities:       [^]Capability,
}

Resource :: struct {
	has_resource:             bool,
	resource:                 Struct,
	connection_details_count: pb_size_t,
	connection_details:       rawptr, // [^]Resource_Connection_Details_Entry, unused here
	ready:                    Ready,
}

State :: struct {
	has_composite:   bool,
	composite:       Resource,
	resources_count: pb_size_t,
	resources:       [^]State_Resources_Entry,
}

State_Resources_Entry :: struct {
	key:       cstring,
	has_value: bool,
	value:     Resource,
}

Run_Function_Request :: struct {
	has_meta:                 bool,
	meta:                     Request_Meta,
	has_observed:             bool,
	observed:                 State,
	has_desired:              bool,
	desired:                  State,
	has_input:                bool,
	input:                    Struct,
	has_context:              bool,
	ctx:                      Struct, // context in the proto; a keyword here
	extra_resources_count:    pb_size_t,
	extra_resources:          rawptr,
	credentials_count:        pb_size_t,
	credentials:              rawptr,
	required_resources_count: pb_size_t,
	required_resources:       rawptr,
	required_schemas_count:   pb_size_t,
	required_schemas:         rawptr,
}

Response_Meta :: struct {
	tag:     cstring,
	has_ttl: bool,
	ttl:     Duration,
}

Requirements :: struct {
	extra_resources_count: pb_size_t,
	extra_resources:       rawptr,
	resources_count:       pb_size_t,
	resources:             rawptr,
	schemas_count:         pb_size_t,
	schemas:               rawptr,
}

Result :: struct {
	severity:   Severity,
	message:    cstring,
	reason:     cstring,
	has_target: bool,
	target:     Target,
}

Condition :: struct {
	type:       cstring,
	status:     Status,
	reason:     cstring,
	message:    cstring,
	has_target: bool,
	target:     Target,
}

Run_Function_Response :: struct {
	has_meta:         bool,
	meta:             Response_Meta,
	has_desired:      bool,
	desired:          State,
	results_count:    pb_size_t,
	results:          [^]Result,
	has_context:      bool,
	ctx:              Struct,
	has_requirements: bool,
	requirements:     Requirements,
	conditions_count: pb_size_t,
	conditions:       [^]Condition,
	has_output:       bool,
	output:           Struct,
}

// ─── nanopb ─────────────────────────────────────────────────────────────────

// pb_istream_t and pb_ostream_t (pb_decode.h, pb_encode.h) with errmsg on
// (PB_NO_ERRMSG unset), filled by pb_istream_from_buffer and
// pb_ostream_from_buffer: a struct returned by value, which Odin lowers on
// wasm32 the way clang does (through a hidden pointer), so the C API is called
// as declared.
Istream :: struct {
	callback:   rawptr,
	state:      rawptr,
	bytes_left: c.size_t,
	errmsg:     cstring,
}

Ostream :: struct {
	callback:      rawptr,
	state:         rawptr,
	max_size:      c.size_t,
	bytes_written: c.size_t,
	errmsg:        cstring,
}

// pb_msgdesc_t, opaque: only ever named by address.
Msg_Desc :: struct {
	opaque: u32,
}

when ODIN_ARCH == .wasm32 {
	// A foreign import whose name ends in .o is, on a wasm target, a set of
	// plain link-time symbols (no wasm import module), and Odin never opens
	// the file: these are the symbols zig links from nanopb and the
	// generated codec.
	foreign import nanopb "nanopb.o"
} else {
	// Natively (the unit tests): the same C half as the objects build.zig
	// compiles for zig build test, named relative to this file.
	foreign import nanopb {
		"../zig-out/lib/guestc/pb_common.o",
		"../zig-out/lib/guestc/pb_decode.o",
		"../zig-out/lib/guestc/pb_encode.o",
		"../zig-out/lib/guestc/run_function.pb.o",
		"../zig-out/lib/guestc/struct.pb.o",
		"../zig-out/lib/guestc/duration.pb.o",
	}
}

@(default_calling_convention = "c")
foreign nanopb {
	pb_istream_from_buffer :: proc(buf: [^]u8, msglen: c.size_t) -> Istream ---
	pb_decode :: proc(stream: ^Istream, fields: ^Msg_Desc, dest_struct: rawptr) -> bool ---
	pb_ostream_from_buffer :: proc(buf: [^]u8, bufsize: c.size_t) -> Ostream ---
	pb_encode :: proc(stream: ^Ostream, fields: ^Msg_Desc, src_struct: rawptr) -> bool ---
	pb_get_encoded_size :: proc(size: ^c.size_t, fields: ^Msg_Desc, src_struct: rawptr) -> bool ---
}

// The message descriptors (src/fnv1/run_function.pb.c). A foreign variable
// on a wasm target must come from a library-less foreign block: it is then a
// plain external symbol, like the procedures above.
foreign {
	@(link_name = "fnv1_RunFunctionRequest_msg")
	Run_Function_Request_msg: Msg_Desc
	@(link_name = "fnv1_RunFunctionResponse_msg")
	Run_Function_Response_msg: Msg_Desc
}

// ─── the layout, held to the C compiler's ───────────────────────────────────

// The wasm32 sizes and offsets (what zig cc computes for the C structs with
// this nanopb configuration); the native layout is held by the codec round
// trip in fn_test.odin.
when ODIN_ARCH == .wasm32 {
#assert(size_of(Struct) == 8)
#assert(size_of(List_Value) == 8)
#assert(size_of(Value) == 16 && offset_of(Value, kind) == 8)
#assert(size_of(Struct_Fields_Entry) == 24 && offset_of(Struct_Fields_Entry, value) == 8)
#assert(size_of(Duration) == 16)
#assert(size_of(Request_Meta) == 12)
#assert(size_of(Resource) == 24 && offset_of(Resource, resource) == 4 && offset_of(Resource, connection_details_count) == 12 && offset_of(Resource, ready) == 20)
#assert(size_of(State) == 36 && offset_of(State, composite) == 4 && offset_of(State, resources_count) == 28 && offset_of(State, resources) == 32)
#assert(size_of(State_Resources_Entry) == 32 && offset_of(State_Resources_Entry, value) == 8)
#assert(size_of(Run_Function_Request) == 152)
#assert(offset_of(Run_Function_Request, meta) == 4 && offset_of(Run_Function_Request, observed) == 20 && offset_of(Run_Function_Request, desired) == 60)
#assert(offset_of(Run_Function_Request, input) == 100 && offset_of(Run_Function_Request, ctx) == 112 && offset_of(Run_Function_Request, extra_resources_count) == 120)
#assert(size_of(Response_Meta) == 24 && offset_of(Response_Meta, ttl) == 8)
#assert(size_of(Requirements) == 24)
#assert(size_of(Result) == 20 && offset_of(Result, target) == 16)
#assert(size_of(Condition) == 24 && offset_of(Condition, target) == 20)
#assert(size_of(Run_Function_Response) == 144)
#assert(offset_of(Run_Function_Response, meta) == 8 && offset_of(Run_Function_Response, desired) == 36 && offset_of(Run_Function_Response, results_count) == 72 && offset_of(Run_Function_Response, results) == 76)
#assert(offset_of(Run_Function_Response, ctx) == 84 && offset_of(Run_Function_Response, requirements) == 96 && offset_of(Run_Function_Response, conditions_count) == 120 && offset_of(Run_Function_Response, conditions) == 124 && offset_of(Run_Function_Response, output) == 132)
#assert(size_of(Istream) == 16 && offset_of(Istream, bytes_left) == 8 && offset_of(Istream, errmsg) == 12)
#assert(size_of(Ostream) == 20 && offset_of(Ostream, max_size) == 8 && offset_of(Ostream, bytes_written) == 12 && offset_of(Ostream, errmsg) == 16)
}
