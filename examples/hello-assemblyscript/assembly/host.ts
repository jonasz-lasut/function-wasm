// The function-wasm host imports (docs/abi.md) and the AssemblyScript abort
// handler. Everything here is what makes the module runnable only under the
// function-wasm runtime; the rest of the guest is ordinary AssemblyScript.

// wasmfn.log: a UTF-8 JSON {"msg", "kv"} record for the host's logger.
@external("wasmfn", "log")
export declare function hostLog(level: u32, ptr: usize, len: u32): void;

// wasmfn.http: a UTF-8 JSON request; the answer is written into a buffer the
// host obtains by calling this module's own wasmfn_alloc re-entrantly, and
// returned as (ptr << 32) | len. 0 means no response.
@external("wasmfn", "http")
export declare function hostHttp(ptr: usize, len: u32): u64;

// abortImpl replaces AssemblyScript's abort (asconfig.json: use abort=...),
// which would otherwise import env.abort - an import the runtime refuses. The
// failure is logged through the host, then the instance traps: the host turns
// that into a fatal result naming the module.
export function abortImpl(message: string | null, fileName: string | null, lineNumber: u32, columnNumber: u32): void {
  let text = "";
  if (message !== null) text = message;
  let location = "";
  if (fileName !== null) location = fileName;
  location += ":" + lineNumber.toString() + ":" + columnNumber.toString();
  const record = '{"msg":"guest abort","kv":["message",' + quote(text) + ',"location",' + quote(location) + "]}";
  const utf8 = String.UTF8.encode(record);
  hostLog(0, changetype<usize>(utf8), <u32>utf8.byteLength);
  unreachable();
}

// quote renders s as a JSON string. Only abort uses it; every other payload
// goes through json-as.
function quote(s: string): string {
  let out = '"';
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c == 0x22) out += '\\"';
    else if (c == 0x5c) out += "\\\\";
    else if (c == 0x0a) out += "\\n";
    else if (c == 0x0d) out += "\\r";
    else if (c == 0x09) out += "\\t";
    else if (c < 0x20) out += "\\u00" + (c >> 4).toString(16) + (c & 15).toString(16);
    else out += String.fromCharCode(c);
  }
  return out + '"';
}
