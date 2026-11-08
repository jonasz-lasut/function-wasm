// Helpers over the mirrored google.protobuf.Struct and Value - the shape of
// the Input's config, of every observed and desired resource and of the
// context - for reading what a request carries and building what a response
// returns. What the builders make is heap memory that is never freed: a
// fresh instance serves each request.
package guest

import "core:strings"

// Reading: every proc takes nil and answers "not there" for it.

// struct_get returns the value of key, or nil.
struct_get :: proc(s: ^Struct, key: string) -> ^Value {
	if s == nil {
		return nil
	}
	for i in 0 ..< s.fields_count {
		e := &s.fields[i]
		if e.key != nil && e.has_value && string(e.key) == key {
			return &e.value
		}
	}
	return nil
}

// struct_lookup walks nested objects by the keys:
//
//	struct_lookup(resource, "metadata", "name")
struct_lookup :: proc(s: ^Struct, keys: ..string) -> ^Value {
	s := s
	v: ^Value
	for key in keys {
		v = struct_get(s, key)
		if v == nil {
			return nil
		}
		s = value_struct(v)
	}
	return v
}

// value_string returns the string of a string value; false for any other kind.
value_string :: proc(v: ^Value) -> (string, bool) {
	if v == nil || v.which_kind != VALUE_STRING_TAG {
		return "", false
	}
	return string(v.kind.string_value), true
}

// value_struct returns the object of a struct value, nil for any other kind.
value_struct :: proc(v: ^Value) -> ^Struct {
	if v == nil || v.which_kind != VALUE_STRUCT_TAG {
		return nil
	}
	return &v.kind.struct_value
}

// Building. The codec reads C strings, so a string is copied with its NUL.

string_value :: proc(s: string) -> Value {
	v: Value
	v.which_kind = VALUE_STRING_TAG
	v.kind.string_value = strings.clone_to_cstring(s)
	return v
}

number_value :: proc(d: f64) -> Value {
	v: Value
	v.which_kind = VALUE_NUMBER_TAG
	v.kind.number_value = d
	return v
}

bool_value :: proc(b: bool) -> Value {
	v: Value
	v.which_kind = VALUE_BOOL_TAG
	v.kind.bool_value = b
	return v
}

struct_value :: proc(s: Struct) -> Value {
	v: Value
	v.which_kind = VALUE_STRUCT_TAG
	v.kind.struct_value = s
	return v
}

// struct_set sets key to v, replacing an existing field of that key; the
// fields array is grown into a new allocation (never freed).
struct_set :: proc(s: ^Struct, key: string, v: Value) {
	for i in 0 ..< s.fields_count {
		if s.fields[i].key != nil && string(s.fields[i].key) == key {
			s.fields[i].has_value = true
			s.fields[i].value = v
			return
		}
	}
	n := int(s.fields_count)
	fields := make([]Struct_Fields_Entry, n + 1)
	if n > 0 {
		copy(fields, s.fields[:n])
	}
	fields[n] = {
		key       = strings.clone_to_cstring(key),
		has_value = true,
		value     = v,
	}
	s.fields = raw_data(fields)
	s.fields_count = pb_size_t(n + 1)
}
