// Hand-written replacement for as-proto-gen's Result (kept by gen-proto, see
// the Makefile): `reason` and `target` are proto3 optional fields with
// explicit presence, which the generator writes unconditionally - a host
// would see reason="" and target=UNSPECIFIED as *set* on every result.

import { Writer, Reader } from "as-proto/assembly";
import { Severity } from "./Severity";
import { Target } from "./Target";

export class Result {
  severity: Severity = 0;
  message: string = "";
  reason: string | null = null;
  hasTarget: bool = false;
  target: Target = 0;

  static encode(message: Result, writer: Writer): void {
    if (message.severity != 0) {
      writer.uint32(8);
      writer.int32(message.severity);
    }
    if (message.message.length != 0) {
      writer.uint32(18);
      writer.string(message.message);
    }
    const reason = message.reason;
    if (reason !== null) {
      writer.uint32(26);
      writer.string(reason);
    }
    if (message.hasTarget) {
      writer.uint32(32);
      writer.int32(message.target);
    }
  }

  static decode(reader: Reader, length: i32): Result {
    const end: usize = length < 0 ? reader.end : reader.ptr + length;
    const message = new Result();

    while (reader.ptr < end) {
      const tag = reader.uint32();
      switch (tag >>> 3) {
        case 1:
          message.severity = reader.int32();
          break;

        case 2:
          message.message = reader.string();
          break;

        case 3:
          message.reason = reader.string();
          break;

        case 4:
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
