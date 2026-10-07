// Unit tests of the guest, run natively by odin test (zig build test) over
// the C half built as a static library: the function over the mirrored
// structs with a fake host for HTTP, and one trip through nanopb's decode,
// run and encode, which holds the native layout to the codec's the way the
// #asserts in fnv1.odin hold the wasm32 one.
#+build !freestanding
package guest

import "core:c"
import "core:mem/virtual"
import "core:testing"

// The guest allocates and never frees (a fresh wasm instance per request
// drops it); each test runs it in an arena of its own.

@(test)
default_greeting :: proc(t: ^testing.T) {
	arena: virtual.Arena
	defer virtual.arena_destroy(&arena)
	context.allocator = virtual.arena_allocator(&arena)

	req := request("my-xr", nil)
	rsp: Run_Function_Response
	testing.expect_value(t, run_function(&req, &rsp), "")
	testing.expect_value(t, greeting_of(&rsp), "hello my-xr")
	testing.expect_value(t, string(rsp.results[0].message), "greeted my-xr")
	testing.expect_value(t, string(rsp.conditions[0].type), "FunctionSuccess")
}

@(test)
configured_greeting_keeps_desired :: proc(t: ^testing.T) {
	arena: virtual.Arena
	defer virtual.arena_destroy(&arena)
	context.allocator = virtual.arena_allocator(&arena)

	config: Struct
	struct_set(&config, "greeting", string_value("hi"))
	req := request("my-xr", &config)
	req.has_desired = true
	state_add_resource(&req.desired, "other", Struct{})
	rsp: Run_Function_Response
	testing.expect_value(t, run_function(&req, &rsp), "")
	testing.expect_value(t, greeting_of(&rsp), "hi my-xr")
	testing.expect_value(t, rsp.desired.resources_count, 2)
}

@(test)
bad_config_is_a_fatal :: proc(t: ^testing.T) {
	arena: virtual.Arena
	defer virtual.arena_destroy(&arena)
	context.allocator = virtual.arena_allocator(&arena)

	config: Struct
	struct_set(&config, "greeting", number_value(7))
	req := request("my-xr", &config)
	rsp: Run_Function_Response
	testing.expect_value(t, run_function(&req, &rsp), "cannot read config: greeting must be a string")
}

@(test)
greeting_from_url_through_the_host :: proc(t: ^testing.T) {
	arena: virtual.Arena
	defer virtual.arena_destroy(&arena)
	context.allocator = virtual.arena_allocator(&arena)

	test_host = proc(url: string) -> (text: string, err: string) {
		if url == "https://greetings.example.com/en" {
			return "howdy", ""
		}
		return "", "sandbox.egress: no rule admits host \"evil.example.com\""
	}
	defer test_host = nil

	ok_config: Struct
	struct_set(&ok_config, "greetingUrl", string_value("https://greetings.example.com/en"))
	ok_req := request("my-xr", &ok_config)
	ok_rsp: Run_Function_Response
	testing.expect_value(t, run_function(&ok_req, &ok_rsp), "")
	testing.expect_value(t, greeting_of(&ok_rsp), "howdy my-xr")

	bad_config: Struct
	struct_set(&bad_config, "greetingUrl", string_value("https://evil.example.com/en"))
	bad_req := request("my-xr", &bad_config)
	bad_rsp: Run_Function_Response
	testing.expect_value(
		t,
		run_function(&bad_req, &bad_rsp),
		"cannot fetch greeting: sandbox.egress: no rule admits host \"evil.example.com\"",
	)
}

// The whole guest half on the wire: a request encoded by the codec, handled
// (decode, run, encode), its response decoded again - what the host does to
// the module, over the native layout of the mirrored structs.
@(test)
request_through_the_codec :: proc(t: ^testing.T) {
	arena: virtual.Arena
	defer virtual.arena_destroy(&arena)
	context.allocator = virtual.arena_allocator(&arena)

	config: Struct
	struct_set(&config, "greeting", string_value("hey"))
	req := request("my-xr", &config)
	size: c.size_t
	testing.expect(t, pb_get_encoded_size(&size, &Run_Function_Request_msg, &req))
	wire := make([]u8, size)
	os := pb_ostream_from_buffer(raw_data(wire), size)
	testing.expect(t, pb_encode(&os, &Run_Function_Request_msg, &req))

	out, ok := handle(wire[:os.bytes_written])
	testing.expect(t, ok)

	rsp: Run_Function_Response
	is := pb_istream_from_buffer(raw_data(out), c.size_t(len(out)))
	testing.expect(t, pb_decode(&is, &Run_Function_Response_msg, &rsp))
	testing.expect_value(t, string(rsp.meta.tag), "hello")
	testing.expect_value(t, rsp.meta.ttl.seconds, 60)
	testing.expect_value(t, greeting_of(&rsp), "hey my-xr")
	testing.expect_value(t, rsp.results[0].severity, Severity.NORMAL)
}

// request is a RunFunctionRequest tagged "hello", observing a composite
// named name, with config as the Input's config block when given.
request :: proc(name: string, config: ^Struct) -> Run_Function_Request {
	req: Run_Function_Request
	req.has_meta = true
	req.meta.tag = "hello"
	metadata: Struct
	struct_set(&metadata, "name", string_value(name))
	resource: Struct
	struct_set(&resource, "apiVersion", string_value("example.org/v1"))
	struct_set(&resource, "kind", string_value("XR"))
	struct_set(&resource, "metadata", struct_value(metadata))
	req.has_observed = true
	req.observed.has_composite = true
	req.observed.composite.has_resource = true
	req.observed.composite.resource = resource
	if config != nil {
		req.has_input = true
		struct_set(&req.input, "config", struct_value(config^))
	}
	return req
}

// greeting_of is data.greeting of the last desired resource.
greeting_of :: proc(rsp: ^Run_Function_Response) -> string {
	n := rsp.desired.resources_count
	cm := &rsp.desired.resources[n - 1].value.resource
	s, _ := value_string(struct_lookup(cm, "data", "greeting"))
	return s
}
