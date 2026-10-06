// The gate over the protocol: it asks Crossplane for the environment's
// EnvironmentConfig through the response's requirements, and once Crossplane
// supplies it, holds every composed resource the earlier steps desired to
// that environment's policy (src/policy.ts). The messages are the ones
// protobuf-es generated from the vendored crossplane proto
// (google.protobuf.Struct arrives as a plain JS object), so this file tests
// under node.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import type { JsonObject } from "@bufbuild/protobuf";
import { DurationSchema } from "@bufbuild/protobuf/wkt";
import {
  Capability,
  ResourceSchema,
  ResponseMetaSchema,
  ResultSchema,
  RunFunctionRequestSchema,
  RunFunctionResponseSchema,
  Severity,
  StateSchema,
  Target,
} from "./gen/run_function_pb.js";
import type {
  Result,
  RunFunctionRequest,
  RunFunctionResponse,
  State,
} from "./gen/run_function_pb.js";
import { enforce, objectAt, parsePolicy } from "./policy.ts";

const DEFAULT_TTL_SECONDS = 60n;

// The key the EnvironmentConfig is requested and supplied under.
export const ENVIRONMENT_CONFIG = "environment-config";

export type Log = (
  level: "debug" | "info",
  msg: string,
  kv: [string, string][],
) => void;

// Runs the gate. Throws a string on a breach or anything it cannot judge;
// handle() turns it into a fatal result, and Crossplane applies nothing from
// a pipeline that ends in one.
export function runFunction(
  req: RunFunctionRequest,
  log: Log,
): RunFunctionResponse {
  const tag = req.meta?.tag ?? "";

  // A Crossplane that says what it supports but leaves out required
  // resources would never answer the requirement below, and the first
  // response - the desired state untouched - would be applied ungated.
  const capabilities = req.meta?.capabilities ?? [];
  if (
    capabilities.includes(Capability.CAPABILITIES) &&
    !capabilities.includes(Capability.REQUIRED_RESOURCES)
  ) {
    throw "this Crossplane cannot supply required resources, which the policy gate reads its rules from";
  }

  const composite = req.observed?.composite?.resource;
  if (composite === undefined) {
    throw "cannot get observed composite resource: none in request";
  }
  const environment = objectAt(composite, ["spec"])?.["environment"];
  if (typeof environment !== "string" || environment === "") {
    throw "the composite resource sets no spec.environment, which chooses the policy it is held to";
  }

  // Crossplane calls the function again with what it asked for, until two
  // calls in a row ask for the same - so every response repeats the
  // requirement, and passes the pipeline's context on.
  const desired = req.desired ?? create(StateSchema, {});
  const rsp = create(RunFunctionResponseSchema, {
    meta: {
      tag,
      ttl: create(DurationSchema, { seconds: DEFAULT_TTL_SECONDS }),
    },
    desired,
    context: req.context,
    requirements: {
      resources: {
        [ENVIRONMENT_CONFIG]: {
          apiVersion: "apiextensions.crossplane.io/v1beta1",
          kind: "EnvironmentConfig",
          match: { case: "matchName", value: environment },
        },
      },
    },
  });

  const supplied = req.requiredResources[ENVIRONMENT_CONFIG];
  if (supplied === undefined) {
    log("info", "Requesting the environment's EnvironmentConfig", [
      ["environment", environment],
    ]);
    return rsp;
  }
  // Crossplane answers a requirement nothing matches with an empty list.
  const environmentConfig = supplied.items[0]?.resource;
  if (environmentConfig === undefined) {
    throw `there is no EnvironmentConfig named ${environment}, which holds the policy of environment ${environment}`;
  }
  const policy = parsePolicy(environment, environmentConfig);

  const violations: string[] = [];
  const warnings: string[] = [];
  let checked = 0;
  let tagsAdded = 0;
  for (const name of Object.keys(desired.resources).sort()) {
    const resource = desired.resources[name]?.resource;
    const outcome = resource && enforce(policy, name, resource);
    if (!outcome) {
      continue;
    }
    checked++;
    tagsAdded += outcome.tagsAdded;
    violations.push(...outcome.violations);
    warnings.push(...outcome.warnings);
  }
  if (violations.length > 0) {
    throw violations.join("; ");
  }

  // Results become events, which render tests cannot assert: the outcome
  // lands in the composite resource's status as well.
  setStatus(desired, {
    environment,
    resources: checked,
    tagsAdded,
    warnings: warnings.length,
  });
  rsp.results = [
    ...warnings.map((message) => result(Severity.WARNING, message)),
    result(
      Severity.NORMAL,
      `held ${count(checked, "composed resource")} to environment ${environment}'s policy: ` +
        `${count(tagsAdded, "tag")} added, ${count(warnings.length, "warning")}`,
    ),
  ];
  log("info", "Held the composed resources to the policy", [
    ["environment", environment],
    ["resources", String(checked)],
    ["warnings", String(warnings.length)],
  ]);
  return rsp;
}

// Writes status.policy onto the desired composite resource, keeping whatever
// status the earlier steps desired.
function setStatus(desired: State, policy: JsonObject) {
  const composite = desired.composite ?? create(ResourceSchema, {});
  const resource = composite.resource ?? {};
  resource["status"] = { ...objectAt(resource, ["status"]), policy };
  composite.resource = resource;
  desired.composite = composite;
}

function count(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

function result(severity: Severity, message: string): Result {
  return create(ResultSchema, { severity, message, target: Target.COMPOSITE });
}

// Decode, run, encode. Every failure becomes a fatal result so the host can
// always decode the reply.
export function handle(input: Uint8Array, log: Log): Uint8Array {
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
    return toBinary(RunFunctionResponseSchema, runFunction(req, log));
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
    results: [result(Severity.FATAL, message)],
  });
  if (req !== undefined) {
    rsp.meta = create(ResponseMetaSchema, {
      tag: req.meta?.tag ?? "",
      ttl: create(DurationSchema, { seconds: DEFAULT_TTL_SECONDS }),
    });
  }
  return rsp;
}
