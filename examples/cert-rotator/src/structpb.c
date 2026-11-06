#include "structpb.h"

#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

const google_protobuf_Value *structpb_get(const google_protobuf_Struct *s, const char *key) {
	for (pb_size_t i = 0; s && key && i < s->fields_count; i++) {
		const google_protobuf_Struct_FieldsEntry *e = &s->fields[i];
		if (e->key && e->has_value && strcmp(e->key, key) == 0) {
			return &e->value;
		}
	}
	return NULL;
}

const google_protobuf_Value *structpb_lookup(const google_protobuf_Struct *s, ...) {
	const google_protobuf_Value *v = NULL;
	va_list keys;
	va_start(keys, s);
	for (const char *key; (key = va_arg(keys, const char *)) != NULL; s = structpb_struct(v)) {
		v = structpb_get(s, key);
		if (!v) {
			break;
		}
	}
	va_end(keys);
	return v;
}

const char *structpb_string(const google_protobuf_Value *v) {
	return v && v->which_kind == google_protobuf_Value_string_value_tag ? v->kind.string_value : NULL;
}

const google_protobuf_Struct *structpb_struct(const google_protobuf_Value *v) {
	return v && v->which_kind == google_protobuf_Value_struct_value_tag ? &v->kind.struct_value : NULL;
}

google_protobuf_Value structpb_string_value(const char *s) {
	google_protobuf_Value v = google_protobuf_Value_init_zero;
	v.which_kind = google_protobuf_Value_string_value_tag;
	v.kind.string_value = (char *)s;
	return v;
}

google_protobuf_Value structpb_number_value(double d) {
	google_protobuf_Value v = google_protobuf_Value_init_zero;
	v.which_kind = google_protobuf_Value_number_value_tag;
	v.kind.number_value = d;
	return v;
}

google_protobuf_Value structpb_bool_value(bool b) {
	google_protobuf_Value v = google_protobuf_Value_init_zero;
	v.which_kind = google_protobuf_Value_bool_value_tag;
	v.kind.bool_value = b;
	return v;
}

google_protobuf_Value structpb_struct_value(google_protobuf_Struct s) {
	google_protobuf_Value v = google_protobuf_Value_init_zero;
	v.which_kind = google_protobuf_Value_struct_value_tag;
	v.kind.struct_value = s;
	return v;
}

bool structpb_set(google_protobuf_Struct *s, const char *key, google_protobuf_Value v) {
	for (pb_size_t i = 0; i < s->fields_count; i++) {
		if (s->fields[i].key && strcmp(s->fields[i].key, key) == 0) {
			s->fields[i].has_value = true;
			s->fields[i].value = v;
			return true;
		}
	}
	google_protobuf_Struct_FieldsEntry *fields = realloc(s->fields, ((size_t)s->fields_count + 1) * sizeof *fields);
	if (!fields) {
		return false;
	}
	fields[s->fields_count].key = (char *)key;
	fields[s->fields_count].has_value = true;
	fields[s->fields_count].value = v;
	s->fields = fields;
	s->fields_count++;
	return true;
}
