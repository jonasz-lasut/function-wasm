// Hand-written replacement for as-proto-gen's RequestMeta (kept by gen-proto,
// see the Makefile): `capabilities` is a repeated enum, which proto3 encoders
// pack into one length-delimited field - crossplane always does. The generator
// only reads the unpacked form, so it would take the block's length for an
// enum value and lose the wire position.

import { Writer, Reader } from "as-proto/assembly";
import { Capability } from "./Capability";

export class RequestMeta {
  tag: string = "";
  capabilities: Array<Capability> = [];

  static encode(message: RequestMeta, writer: Writer): void {
    if (message.tag.length != 0) {
      writer.uint32(10);
      writer.string(message.tag);
    }
    const capabilities = message.capabilities;
    if (capabilities.length != 0) {
      writer.uint32(18);
      writer.fork();
      for (let i = 0; i < capabilities.length; i++) {
        writer.int32(capabilities[i]);
      }
      writer.ldelim();
    }
  }

  static decode(reader: Reader, length: i32): RequestMeta {
    const end: usize = length < 0 ? reader.end : reader.ptr + length;
    const message = new RequestMeta();

    while (reader.ptr < end) {
      const tag = reader.uint32();
      switch (tag >>> 3) {
        case 1:
          message.tag = reader.string();
          break;

        case 2:
          if ((tag & 7) == 2) {
            // Packed: one length-delimited block of varints. Read the length
            // before taking reader.ptr - operands evaluate left to right.
            const packedLength: usize = reader.uint32();
            const packedEnd: usize = reader.ptr + packedLength;
            while (reader.ptr < packedEnd) {
              message.capabilities.push(reader.int32());
            }
          } else {
            message.capabilities.push(reader.int32());
          }
          break;

        default:
          reader.skipType(tag & 7);
          break;
      }
    }

    return message;
  }
}
