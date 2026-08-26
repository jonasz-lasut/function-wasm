// Helpers over the nanopb types of google.protobuf.Struct and Value - the
// shape of the Input's config, of every observed and desired resource and of
// the context - for reading what a request carries and building what a
// response returns. What the builders make is heap memory that is never freed
// (a fresh instance serves each request); they return false when out of
// memory.
#ifndef STRUCTPB_H
#define STRUCTPB_H

#include <stdbool.h>

#include "google/protobuf/struct.pb.h"

// Reading: every function takes NULL and returns NULL for "not there".

// structpb_get returns the value of key, or NULL.
const google_protobuf_Value *structpb_get(const google_protobuf_Struct *s, const char *key);

// structpb_lookup walks nested objects by the NULL-terminated keys:
//
//	structpb_lookup(resource, "metadata", "name", NULL)
const google_protobuf_Value *structpb_lookup(const google_protobuf_Struct *s, ...);

// structpb_string returns the string of a string value, NULL for any other kind.
const char *structpb_string(const google_protobuf_Value *v);

// structpb_struct returns the object of a struct value, NULL for any other kind.
const google_protobuf_Struct *structpb_struct(const google_protobuf_Value *v);

// Building. The string value keeps a reference to s, it does not copy it.
google_protobuf_Value structpb_string_value(const char *s);
google_protobuf_Value structpb_number_value(double d);
google_protobuf_Value structpb_bool_value(bool b);
google_protobuf_Value structpb_struct_value(google_protobuf_Struct s);

// structpb_set sets key to v, replacing an existing field of that key.
bool structpb_set(google_protobuf_Struct *s, const char *key, google_protobuf_Value v);

#endif
