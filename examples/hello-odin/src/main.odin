// The hello-odin guest: a Crossplane composition function in Odin, compiled
// to a wasm32 object that zig links (with nanopb and the c flavour's
// bindings and glue) into the core module guestfn build wraps into an ABI
// v2 component, run by function-wasm. It composes a ConfigMap greeting the
// composite resource.
//
// run_function is ordinary Odin over the nanopb structs src/fnv1.odin
// mirrors; the world's run export and the log / wasi:http host imports live
// in wasmfn.odin, behind the bindings wit-bindgen generated from wit/
// (src/gen).
package guest

import "core:strings"

// run_function composes a response to req into rsp, which the glue hands
// over zero-initialised except for meta (the request's tag and a 60 s TTL),
// and returns "" - or the message of a fatal result, in which case the glue
// discards rsp.
run_function :: proc(req: ^Run_Function_Request, rsp: ^Run_Function_Response) -> (fatal: string) {
	tag := string(req.meta.tag) if req.has_meta && req.meta.tag != nil else ""
	log_info("Running function", {"tag", tag})

	greeting := "hello"
	switch g, r := config_string(req, "greeting"); r {
	case .Not_String:
		return "cannot read config: greeting must be a string"
	case .String:
		greeting = g
	case .Absent:
	}
	// greetingUrl fetches the greeting through the host instead - the
	// requires.egress grant of the module's manifest decides whether it may.
	switch url, r := config_string(req, "greetingUrl"); r {
	case .Not_String:
		return "cannot read config: greetingUrl must be a string"
	case .String:
		text, err := http_get_text(url)
		if err != "" {
			return strings.concatenate({"cannot fetch greeting: ", err})
		}
		greeting = text
	case .Absent:
	}

	name, has_name := observed_name(req)
	if !has_name {
		return "cannot get observed composite resource: none in request"
	}

	// The ConfigMap greeting the composite, added to the desired state the
	// request carried.
	data: Struct
	struct_set(&data, "greeting", string_value(strings.concatenate({greeting, " ", name})))
	cm: Struct
	struct_set(&cm, "apiVersion", string_value("v1"))
	struct_set(&cm, "kind", string_value("ConfigMap"))
	struct_set(&cm, "data", struct_value(data))
	if req.has_desired {
		rsp.desired = req.desired
	}
	rsp.has_desired = true
	state_add_resource(&rsp.desired, "greeting", cm)

	result := new(Result)
	result^ = {
		severity   = .NORMAL,
		message    = strings.clone_to_cstring(strings.concatenate({"greeted ", name})),
		has_target = true,
		target     = .COMPOSITE,
	}
	rsp.results = ([^]Result)(result)
	rsp.results_count = 1
	condition := new(Condition)
	condition^ = {
		type       = "FunctionSuccess",
		status     = .TRUE,
		reason     = "Success",
		has_target = true,
		target     = .COMPOSITE_AND_CLAIM,
	}
	rsp.conditions = ([^]Condition)(condition)
	rsp.conditions_count = 1

	log_debug("Composed the greeting", {"name", name}, {"greeting", greeting})
	return ""
}

Config_Result :: enum {
	Absent,
	String,
	Not_String,
}

// config_string reads a string field of the Input's config block.
config_string :: proc(req: ^Run_Function_Request, key: string) -> (value: string, result: Config_Result) {
	if !req.has_input {
		return "", .Absent
	}
	v := struct_lookup(&req.input, "config", key)
	if v == nil {
		return "", .Absent
	}
	s, ok := value_string(v)
	if !ok {
		return "", .Not_String
	}
	return s, .String
}

observed_name :: proc(req: ^Run_Function_Request) -> (string, bool) {
	if !req.has_observed || !req.observed.has_composite || !req.observed.composite.has_resource {
		return "", false
	}
	return value_string(struct_lookup(&req.observed.composite.resource, "metadata", "name"))
}

// state_add_resource adds resource under key to the state's resources, in a
// new array: a state copied from the request is never modified in place.
state_add_resource :: proc(state: ^State, key: string, resource: Struct) {
	n := int(state.resources_count)
	entries := make([]State_Resources_Entry, n + 1)
	if n > 0 {
		copy(entries, state.resources[:n])
	}
	entries[n] = {
		key       = strings.clone_to_cstring(key),
		has_value = true,
		value     = {has_resource = true, resource = resource},
	}
	state.resources = raw_data(entries)
	state.resources_count = pb_size_t(n + 1)
}
