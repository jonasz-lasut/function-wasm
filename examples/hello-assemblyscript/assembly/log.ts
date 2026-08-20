// Logging through the host's logger (wasmfn.log): logInfo("msg", ["k", "v"]).
// The host attaches the module reference and digest and logs the record
// through its own logger.

import { JSON } from "json-as";

import { hostLog } from "./host";


@json
class LogRecord {
  msg: string = "";
  kv: string[] = [];
}

export function logInfo(msg: string, kv: string[]): void {
  emit(0, msg, kv);
}

export function logDebug(msg: string, kv: string[]): void {
  emit(1, msg, kv);
}

function emit(level: u32, msg: string, kv: string[]): void {
  const record = new LogRecord();
  record.msg = msg;
  record.kv = kv;
  const utf8 = String.UTF8.encode(JSON.stringify(record));
  hostLog(level, changetype<usize>(utf8), <u32>utf8.byteLength);
}
