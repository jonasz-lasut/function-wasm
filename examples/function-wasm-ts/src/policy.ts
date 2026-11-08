// The organisation's policy for one environment, as its EnvironmentConfig
// states it under data.policy, and what it does to a composed resource. Plain
// JSON in, plain JSON out: resources arrive as protobuf-es Struct values,
// which are ordinary JS objects, so this file knows nothing of the protocol.

import type { JsonObject, JsonValue } from "@bufbuild/protobuf";

export interface Policy {
  environment: string;
  // Tags every composed resource carries; a team may set its own value for
  // one, which the gate keeps and reports.
  mandatoryTags: Record<string, string>;
  // Absent means the environment does not restrict the field.
  allowedRegions?: string[];
  allowedInstanceClasses?: string[];
}

// What the gate did to one composed resource.
export interface Outcome {
  // Breaches of the policy, each fatal for the whole pipeline.
  violations: string[];
  // Softer findings: the team's choice stands, but someone should know.
  warnings: string[];
  tagsAdded: number;
}

// Reads data.policy of the environment's EnvironmentConfig. A malformed
// policy throws a message naming the field: a gate that cannot read its rules
// must not let anything through.
export function parsePolicy(
  environment: string,
  environmentConfig: JsonObject,
): Policy {
  const where = `EnvironmentConfig ${environment}`;
  const policy = objectAt(environmentConfig, ["data", "policy"]);
  if (policy === undefined) {
    throw `${where} has no data.policy`;
  }
  const tags = policy["mandatoryTags"] ?? {};
  if (!isObject(tags) || !Object.values(tags).every(isString)) {
    throw `${where}: data.policy.mandatoryTags must map tag names to strings`;
  }
  return {
    environment,
    mandatoryTags: tags as Record<string, string>,
    allowedRegions: stringList(policy, "allowedRegions", where),
    allowedInstanceClasses: stringList(policy, "allowedInstanceClasses", where),
  };
}

// Checks one composed resource against the policy and adds the mandatory
// tags it lacks to its spec.forProvider.tags, in place. A resource without
// spec.forProvider is not a managed resource and is left alone.
export function enforce(
  policy: Policy,
  name: string,
  resource: JsonObject,
): Outcome | undefined {
  const forProvider = objectAt(resource, ["spec", "forProvider"]);
  if (forProvider === undefined) {
    return undefined;
  }
  const what = `${resource["kind"] ?? "resource"} "${name}"`;
  const outcome: Outcome = { violations: [], warnings: [], tagsAdded: 0 };

  // Only a field the resource sets can be judged: an unset region is the
  // provider configuration's, and most kinds have no instance class at all.
  const allowed: [string, string[] | undefined][] = [
    ["region", policy.allowedRegions],
    ["instanceClass", policy.allowedInstanceClasses],
  ];
  for (const [field, allowList] of allowed) {
    const value = forProvider[field];
    if (allowList === undefined || value === undefined || value === null) {
      continue;
    }
    if (typeof value !== "string" || !allowList.includes(value)) {
      outcome.violations.push(
        `${what} sets spec.forProvider.${field} to ${JSON.stringify(value)}, ` +
          `which environment ${policy.environment} does not allow ` +
          `(allowed: ${allowList.join(", ")})`,
      );
    }
  }

  const tags = forProvider["tags"] ?? {};
  if (!isObject(tags)) {
    outcome.violations.push(
      `${what} sets spec.forProvider.tags to something other than a map`,
    );
    return outcome;
  }
  // Sorted, so the warnings come out in the same order whatever order the
  // EnvironmentConfig's map arrived in.
  const mandatory = Object.entries(policy.mandatoryTags).sort(([a], [b]) =>
    a < b ? -1 : a > b ? 1 : 0,
  );
  for (const [key, value] of mandatory) {
    const team = tags[key];
    if (team === undefined || team === null) {
      tags[key] = value;
      outcome.tagsAdded++;
    } else if (team !== value) {
      outcome.warnings.push(
        `${what} keeps the team's tag ${key}=${team} over environment ` +
          `${policy.environment}'s ${key}=${value}`,
      );
    }
  }
  if (outcome.tagsAdded > 0) {
    forProvider["tags"] = tags;
  }
  return outcome;
}

// Follows a path of object keys; anything that is not an object on the way
// ends it.
export function objectAt(
  obj: JsonObject | undefined,
  path: string[],
): JsonObject | undefined {
  let at: JsonValue | undefined = obj;
  for (const key of path) {
    if (!isObject(at)) {
      return undefined;
    }
    at = at[key];
  }
  return isObject(at) ? at : undefined;
}

function stringList(
  policy: JsonObject,
  key: string,
  where: string,
): string[] | undefined {
  const v = policy[key];
  if (v === undefined || v === null) {
    return undefined;
  }
  if (!Array.isArray(v) || !v.every(isString)) {
    throw `${where}: data.policy.${key} must be a list of strings`;
  }
  return v as string[];
}

function isObject(v: JsonValue | undefined): v is JsonObject {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isString(v: JsonValue): v is string {
  return typeof v === "string";
}
