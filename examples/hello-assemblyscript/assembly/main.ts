// The module's entry: what asc compiles, and whose exported functions are the
// module's exports. Not named index.ts: json-as's transform (v1.6.0) demotes
// any source whose internal path is exactly "assembly/index" to a library
// (meaning it to match its own package entry), which silently drops a user
// entry's exports. The logic lives in fn.ts (runFunction), the wire handling
// in abi.ts, the host imports in host.ts.

import { handle } from "./abi";

// wasmfn_alloc hands the host buffers: the request, and wasmfn.http answers -
// for those it is called re-entrantly while wasmfn_run is on the stack, which
// a bump allocator serves without ceremony. The stub runtime never collects
// or moves, so every buffer stays put for the instance's one request.
export function wasmfn_alloc(size: u32): u32 {
  return <u32>heap.alloc(size > 0 ? <usize>size : 1);
}

// wasmfn_run decodes the request at ptr/len, runs the function and returns
// the encoded response as (ptr << 32) | len.
export function wasmfn_run(ptr: u32, len: u32): u64 {
  const out = handle(ptr, len);
  return (<u64>out.dataStart << 32) | <u64>out.length;
}
