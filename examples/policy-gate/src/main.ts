// The world wiring, wasm-only: componentize-js maps the world's root-level
// `log` import to a default import from a module named after it (typed in
// src/log.d.ts), and this module's exported `run` implements the world's
// export. `run` is declared sync in this guest's wit (componentize-js
// cannot async-lift a custom world's export yet; a sync-lifted function
// satisfies the runtime's async world), and the gate has nothing to await:
// what it reads arrives in the request, as required resources.

import log from "log";

import { handle } from "./fn.ts";

export function run(request: Uint8Array): Uint8Array {
  return handle(request, log);
}
