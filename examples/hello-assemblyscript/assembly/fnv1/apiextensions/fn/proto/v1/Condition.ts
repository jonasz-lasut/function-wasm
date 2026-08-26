// Hand-written replacement for as-proto-gen's Condition (kept by gen-proto,
// see the Makefile): `message` and `target` are proto3 optional fields with
// explicit presence, which the generator writes unconditionally.

import { Writer, Reader } from "as-proto/assembly";
import { Status } from "./Status";
import { Target } from "./Target";

export class Condition {
  type: string = "";
  status: Status = 0;
  reason: string = "";
  message: string | null = null;
  hasTarget: bool = false;
  target: Target = 0;

  static encode(message: Condition, writer: Writer): void {
    if (message.type.length != 0) {
      writer.uint32(10);
      writer.string(message.type);
    }
    if (message.status != 0) {
      writer.uint32(16);
      writer.int32(message.status);
    }
    if (message.reason.length != 0) {
      writer.uint32(26);
      writer.string(message.reason);
    }
    const text = message.message;
    if (text !== null) {
      writer.uint32(34);
      writer.string(text);
    }
    if (message.hasTarget) {
      writer.uint32(40);
      writer.int32(message.target);
    }
  }

  static decode(reader: Reader, length: i32): Condition {
    const end: usize = length < 0 ? reader.end : reader.ptr + length;
    const message = new Condition();

    while (reader.ptr < end) {
      const tag = reader.uint32();
      switch (tag >>> 3) {
        case 1:
          message.type = reader.string();
          break;

        case 2:
          message.status = reader.int32();
          break;

        case 3:
          message.reason = reader.string();
          break;

        case 4:
          message.message = reader.string();
          break;

        case 5:
          message.hasTarget = true;
          message.target = reader.int32();
          break;

        default:
          reader.skipType(tag & 7);
          break;
      }
    }

    return message;
  }
}
