// The guest half of ABI v1 (docs/abi.md) behind the exports in index.ts:
// handle decodes the request, calls runFunction and encodes the response.
// Every failure becomes a fatal result, so the host can always decode the
// reply. A fresh instance serves each request, so nothing is ever freed.

import { Protobuf } from "as-proto/assembly";

import { Result } from "./fnv1/apiextensions/fn/proto/v1/Result";
import { RunFunctionRequest } from "./fnv1/apiextensions/fn/proto/v1/RunFunctionRequest";
import { RunFunctionResponse } from "./fnv1/apiextensions/fn/proto/v1/RunFunctionResponse";
import { Severity } from "./fnv1/apiextensions/fn/proto/v1/Severity";
import { Target } from "./fnv1/apiextensions/fn/proto/v1/Target";
import { responseMeta, runFunction } from "./fn";

export function handle(ptr: u32, len: u32): Uint8Array {
  const input = new Uint8Array(<i32>len);
  memory.copy(input.dataStart, <usize>ptr, <usize>len);
  const req = Protobuf.decode<RunFunctionRequest>(input, RunFunctionRequest.decode);
  const meta = req.meta;
  const tag = meta === null ? "" : meta.tag;
  const outcome = runFunction(req);
  const err = outcome.err;
  let rsp: RunFunctionResponse;
  if (err !== null) {
    rsp = fatal(tag, err);
  } else {
    rsp = outcome.rsp === null ? fatal(tag, "the function returned nothing") : (outcome.rsp as RunFunctionResponse);
  }
  return Protobuf.encode(rsp, RunFunctionResponse.encode);
}

// fatal is a fresh response carrying one fatal result.
export function fatal(tag: string, message: string): RunFunctionResponse {
  const rsp = new RunFunctionResponse();
  rsp.meta = responseMeta(tag);
  const result = new Result();
  result.severity = Severity.SEVERITY_FATAL;
  result.message = message;
  result.hasTarget = true;
  result.target = Target.TARGET_COMPOSITE;
  rsp.results = [result];
  return rsp;
}
