// Helpers over google.protobuf.Struct and Value - the shape of the Input's
// config, of every observed and desired resource and of the context.

import { Struct } from "./fnv1/google/protobuf/Struct";
import { Value, ValueKind } from "./fnv1/google/protobuf/Value";

// field returns the value of key, or null.
export function field(s: Struct | null, key: string): Value | null {
  if (s === null) return null;
  const fields = s.fields;
  return fields.has(key) ? fields.get(key) : null;
}

// stringValue returns the string of a string value, null for any other kind.
export function stringValue(v: Value | null): string | null {
  return v !== null && v.kind == ValueKind.STRING ? v.stringValue : null;
}

// structValue returns the object of a struct value, null for any other kind.
export function structValue(v: Value | null): Struct | null {
  return v !== null && v.kind == ValueKind.STRUCT ? v.structValue : null;
}

// object builds a Struct from alternating key/Value pairs.
export function object(entries: Array<Entry>): Struct {
  const s = new Struct();
  for (let i = 0; i < entries.length; i++) {
    s.fields.set(entries[i].key, entries[i].value);
  }
  return s;
}

export class Entry {
  constructor(
    public key: string,
    public value: Value,
  ) {}
}
