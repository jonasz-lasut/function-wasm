// The world wiring, wasm-only: componentize-js maps the world's root-level
// `log` import to a default import from a module named after it (typed in
// src/log.d.ts), and this module's exported `run` implements the world's
// export. `run` is declared sync in this guest's wit (componentize-js
// cannot async-lift a custom world's export yet; a sync-lifted function
// satisfies the runtime's async world) - the TypeScript still awaits
// freely, componentize-js resolves the promise before the export returns.
// fetch() rides wasi:http@0.2 into the host's egress policy.

import log from "log";

import { handle } from "./fn.ts";

// GETs a URL through the host and returns the trimmed body - the fetch()
// counterpart of the other guests' get_text helpers.
async function fetchText(url: string): Promise<string> {
  const rsp = await fetch(url);
  if (rsp.status !== 200) {
    throw new Error(`GET ${url}: status ${rsp.status}`);
  }
  return (await rsp.text()).trim();
}

export async function run(request: Uint8Array): Promise<Uint8Array> {
  return await handle(request, fetchText, log);
}
