// Hand-written replacement for as-proto-gen's Value (kept by gen-proto, see
// the Makefile): the generator writes every member of the `kind` oneof
// unconditionally, so an encoded Value would decode as whichever member the
// wire carries last - a string value would come back as a bool. A Value also
// has to remember which member was set (`kind`), so a guest can hand state it
// did not build - the request's desired resources - back to the host
// unchanged: a decoded empty string is indistinguishable from an unset one
// without it.

import { Writer, Reader } from "as-proto/assembly";
import { Struct } from "./Struct";
import { ListValue } from "./ListValue";
import { NullValue } from "./NullValue";

// The oneof member a Value carries; the values are the field numbers.
export enum ValueKind {
  NONE = 0,
  NULL = 1,
  NUMBER = 2,
  STRING = 3,
  BOOL = 4,
  STRUCT = 5,
  LIST = 6,
}

export class Value {
  kind: ValueKind = ValueKind.NONE;
  nullValue: NullValue = NullValue.NULL_VALUE;
  numberValue: f64 = 0.0;
  stringValue: string = "";
  boolValue: bool = false;
  structValue: Struct | null = null;
  listValue: ListValue | null = null;

  static ofNull(): Value {
    const value = new Value();
    value.kind = ValueKind.NULL;
    return value;
  }

  static ofNumber(numberValue: f64): Value {
    const value = new Value();
    value.kind = ValueKind.NUMBER;
    value.numberValue = numberValue;
    return value;
  }

  static ofString(stringValue: string): Value {
    const value = new Value();
    value.kind = ValueKind.STRING;
    value.stringValue = stringValue;
    return value;
  }

  static ofBool(boolValue: bool): Value {
    const value = new Value();
    value.kind = ValueKind.BOOL;
    value.boolValue = boolValue;
    return value;
  }

  static ofStruct(structValue: Struct): Value {
    const value = new Value();
    value.kind = ValueKind.STRUCT;
    value.structValue = structValue;
    return value;
  }

  static ofList(listValue: ListValue): Value {
    const value = new Value();
    value.kind = ValueKind.LIST;
    value.listValue = listValue;
    return value;
  }

  static encode(message: Value, writer: Writer): void {
    switch (message.kind) {
      case ValueKind.NULL: {
        writer.uint32(8);
        writer.int32(message.nullValue);
        break;
      }
      case ValueKind.NUMBER: {
        writer.uint32(17);
        writer.double(message.numberValue);
        break;
      }
      case ValueKind.STRING: {
        writer.uint32(26);
        writer.string(message.stringValue);
        break;
      }
      case ValueKind.BOOL: {
        writer.uint32(32);
        writer.bool(message.boolValue);
        break;
      }
      case ValueKind.STRUCT: {
        const structValue = message.structValue;
        if (structValue !== null) {
          writer.uint32(42);
          writer.fork();
          Struct.encode(structValue, writer);
          writer.ldelim();
        }
        break;
      }
      case ValueKind.LIST: {
        const listValue = message.listValue;
        if (listValue !== null) {
          writer.uint32(50);
          writer.fork();
          ListValue.encode(listValue, writer);
          writer.ldelim();
        }
        break;
      }
      default:
        break;
    }
  }

  static decode(reader: Reader, length: i32): Value {
    const end: usize = length < 0 ? reader.end : reader.ptr + length;
    const message = new Value();

    while (reader.ptr < end) {
      const tag = reader.uint32();
      switch (tag >>> 3) {
        case 1:
          message.kind = ValueKind.NULL;
          message.nullValue = reader.int32();
          break;

        case 2:
          message.kind = ValueKind.NUMBER;
          message.numberValue = reader.double();
          break;

        case 3:
          message.kind = ValueKind.STRING;
          message.stringValue = reader.string();
          break;

        case 4:
          message.kind = ValueKind.BOOL;
          message.boolValue = reader.bool();
          break;

        case 5:
          message.kind = ValueKind.STRUCT;
          message.structValue = Struct.decode(reader, reader.uint32());
          break;

        case 6:
          message.kind = ValueKind.LIST;
          message.listValue = ListValue.decode(reader, reader.uint32());
          break;

        default:
          reader.skipType(tag & 7);
          break;
      }
    }

    return message;
  }
}
