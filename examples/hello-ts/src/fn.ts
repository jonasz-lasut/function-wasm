// The hello-ts guest's logic: the same greeting function as every example,
// over the protobuf messages protobuf-es generated from the vendored
// crossplane proto (google.protobuf.Struct arrives as a plain JS object).
// `fetchText` resolves config.greetingUrl - fetch() over wasi:http on the
// wasm target, a test double natively - so this file tests under node.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import type { JsonObject, JsonValue } from "@bufbuild/protobuf";
import { DurationSchema } from "@bufbuild/protobuf/wkt";
import {
  ResourceSchema,
  ResponseMetaSchema,
  RunFunctionRequestSchema,
  RunFunctionResponseSchema,
  Severity,
  StateSchema,
  Status,
  Target,
} from "./gen/run_function_pb.js";
import type {
  RunFunctionRequest,
  RunFunctionResponse,
} from "./gen/run_function_pb.js";

const DEFAULT_TTL_SECONDS = 60n;

export type FetchText = (url: string) => Promise<string>;
export type Log = (
  level: "debug" | "info",
  msg: string,
  kv: [string, string][],
) => void;

// Adds a ConfigMap greeting the composite resource to the desired state.
// Throws a string on failure; handle() turns it into a fatal result.
export async function runFunction(
  req: RunFunctionRequest,
  fetchText: FetchText,
  log: Log,
): Promise<RunFunctionResponse> {
  const tag = req.meta?.tag ?? "";
  log("info", "Running function", [["tag", tag]]);

  const config = structField(req.input, "config");
  let greeting =
    stringField(config, "greeting", "cannot read config") ?? "hello";
  // greetingUrl fetches the greeting through the host instead — the
  // requires.egress grant of the module's manifest decides whether it may.
  const url = stringField(config, "greetingUrl", "cannot read config");
  if (url !== undefined) {
    try {
      greeting = await fetchText(url);
    } catch (e) {
      throw `cannot fetch greeting: ${e instanceof Error ? e.message : e}`;
    }
  }

  const composite = req.observed?.composite?.resource;
  if (composite === undefined) {
    throw "cannot get observed composite resource: none in request";
  }
  const name =
    stringField(structField(composite, "metadata"), "name", "cannot read metadata") ?? "";

  const desired = req.desired ?? create(StateSchema, {});
  desired.resources["greeting"] = create(ResourceSchema, {
    resource: {
      apiVersion: "v1",
      kind: "ConfigMap",
      data: { greeting: `${greeting} ${name}` },
    },
  });

  return create(RunFunctionResponseSchema, {
    meta: {
      tag,
      ttl: create(DurationSchema, { seconds: DEFAULT_TTL_SECONDS }),
    },
    desired,
    results: [
      {
        severity: Severity.NORMAL,
        message: `greeted ${name}`,
        target: Target.COMPOSITE,
      },
    ],
    conditions: [
      {
        type: "FunctionSuccess",
        status: Status.CONDITION_TRUE,
        reason: "Success",
        target: Target.COMPOSITE_AND_CLAIM,
      },
    ],
  });
}

// Reads a Struct field's sub-object; Struct fields are plain JS objects.
function structField(
  struct: JsonObject | undefined,
  key: string,
): JsonObject | undefined {
  const v: JsonValue | undefined = struct?.[key];
  return typeof v === "object" && v !== null && !Array.isArray(v)
    ? v
    : undefined;
}

// Reads a string field of an object, refusing non-strings the way the other
// guests word it.
function stringField(
  obj: JsonObject | undefined,
  key: string,
  context: string,
): string | undefined {
  const v: JsonValue | undefined = obj?.[key];
  if (v === undefined || v === null) {
    return undefined;
  }
  if (typeof v !== "string") {
    throw `${context}: ${key} must be a string`;
  }
  return v;
}

// Decode, run, encode. Every failure becomes a fatal result so the host can
// always decode the reply.
export async function handle(
  input: Uint8Array,
  fetchText: FetchText,
  log: Log,
): Promise<Uint8Array> {
  let req: RunFunctionRequest;
  try {
    req = fromBinary(RunFunctionRequestSchema, input);
  } catch (e) {
    return toBinary(
      RunFunctionResponseSchema,
      fatal(
        undefined,
        `cannot decode RunFunctionRequest: ${e instanceof Error ? e.message : e}`,
      ),
    );
  }
  try {
    return toBinary(
      RunFunctionResponseSchema,
      await runFunction(req, fetchText, log),
    );
  } catch (e) {
    return toBinary(
      RunFunctionResponseSchema,
      fatal(req, e instanceof Error ? e.message : String(e)),
    );
  }
}

function fatal(
  req: RunFunctionRequest | undefined,
  message: string,
): RunFunctionResponse {
  const rsp = create(RunFunctionResponseSchema, {
    results: [
      {
        severity: Severity.FATAL,
        message,
        target: Target.COMPOSITE,
      },
    ],
  });
  if (req !== undefined) {
    rsp.meta = create(ResponseMetaSchema, {
      tag: req.meta?.tag ?? "",
      ttl: create(DurationSchema, { seconds: DEFAULT_TTL_SECONDS }),
    });
  }
  return rsp;
}
