// Native tests for the gate, under node --test (node strips the types): the
// requirement round trip Crossplane drives, the tags, every fatal path and
// the warnings. The log double stands in for the world's import.

import assert from "node:assert/strict";
import { test } from "node:test";

import { create, equals, fromBinary, toBinary } from "@bufbuild/protobuf";
import type { JsonObject, JsonValue } from "@bufbuild/protobuf";
import {
  Capability,
  RequirementsSchema,
  ResourceSchema,
  RunFunctionRequestSchema,
  RunFunctionResponseSchema,
  Severity,
} from "../src/gen/run_function_pb.js";
import type {
  RunFunctionRequest,
  RunFunctionResponse,
} from "../src/gen/run_function_pb.js";
import { ENVIRONMENT_CONFIG, handle, runFunction } from "../src/fn.ts";

const log = () => {};

const PROD_POLICY = {
  mandatoryTags: {
    "cost-center": "4200",
    "data-classification": "confidential",
  },
  allowedRegions: ["eu-central-1", "eu-west-1"],
  allowedInstanceClasses: ["db.r6g.large", "db.r6g.xlarge"],
};

function environmentConfig(name: string, data: JsonObject): JsonObject {
  return {
    apiVersion: "apiextensions.crossplane.io/v1beta1",
    kind: "EnvironmentConfig",
    metadata: { name },
    data,
  };
}

function instance(forProvider: JsonObject): JsonObject {
  return {
    apiVersion: "rds.aws.m.upbound.io/v1beta1",
    kind: "Instance",
    spec: { forProvider },
  };
}

interface Call {
  spec?: JsonObject;
  resources?: Record<string, JsonObject>;
  // undefined: the first call, before Crossplane fetched anything; null:
  // Crossplane fetched and found no EnvironmentConfig.
  supplied?: JsonObject | null;
  capabilities?: Capability[];
}

function request(call: Call): RunFunctionRequest {
  const resources: Record<string, { resource: JsonObject }> = {};
  for (const [name, resource] of Object.entries(call.resources ?? {})) {
    resources[name] = { resource };
  }
  const requiredResources: Record<
    string,
    { items: { resource: JsonObject }[] }
  > = {};
  if (call.supplied !== undefined) {
    requiredResources[ENVIRONMENT_CONFIG] = {
      items: call.supplied === null ? [] : [{ resource: call.supplied }],
    };
  }
  return create(RunFunctionRequestSchema, {
    meta: { tag: "gate", capabilities: call.capabilities ?? [] },
    observed: {
      composite: {
        resource: {
          apiVersion: "data.example.org/v1alpha1",
          kind: "Database",
          metadata: { name: "orders", namespace: "default" },
          spec: call.spec ?? { environment: "prod" },
        },
      },
    },
    desired: { resources },
    context: { "example.org/previous-step": "kept" },
    requiredResources,
  });
}

// The second call of the round trip: the request carries the prod policy.
function supplied(call: Call): RunFunctionResponse {
  return runFunction(
    request({
      supplied: environmentConfig("prod", { policy: PROD_POLICY }),
      ...call,
    }),
    log,
  );
}

function forProvider(rsp: RunFunctionResponse, name: string): JsonObject {
  const resource = rsp.desired?.resources[name]?.resource as JsonObject;
  return (resource.spec as JsonObject).forProvider as JsonObject;
}

function policyStatus(rsp: RunFunctionResponse): JsonValue {
  const xr = rsp.desired?.composite?.resource as JsonObject;
  return (xr.status as JsonObject).policy;
}

function rejects(call: Call, message: string) {
  assert.throws(
    () => runFunction(request(call), log),
    (e: unknown) => {
      assert.equal(e, message);
      return true;
    },
  );
}

test("the first call only asks for the environment's EnvironmentConfig", () => {
  const db = instance({ region: "us-east-1", tags: { team: "orders" } });
  const req = request({ resources: { db } });
  const rsp = runFunction(req, log);

  const selector = rsp.requirements?.resources[ENVIRONMENT_CONFIG];
  assert.equal(selector?.apiVersion, "apiextensions.crossplane.io/v1beta1");
  assert.equal(selector?.kind, "EnvironmentConfig");
  assert.deepEqual(selector?.match, { case: "matchName", value: "prod" });
  // Nothing is judged yet: not even the region the prod policy forbids.
  assert.deepEqual(rsp.desired?.resources["db"]?.resource, {
    apiVersion: "rds.aws.m.upbound.io/v1beta1",
    kind: "Instance",
    spec: { forProvider: { region: "us-east-1", tags: { team: "orders" } } },
  });
  assert.equal(rsp.desired?.composite, undefined);
  assert.deepEqual(rsp.results, []);
  assert.deepEqual(rsp.context, { "example.org/previous-step": "kept" });
  assert.equal(rsp.meta?.tag, "gate");
});

test("every call repeats the same requirement, so Crossplane stops calling", () => {
  const first = runFunction(request({}), log);
  const second = supplied({});
  assert.ok(first.requirements);
  assert.ok(
    equals(RequirementsSchema, first.requirements, second.requirements!),
  );
});

test("the environment chooses the EnvironmentConfig", () => {
  const rsp = runFunction(request({ spec: { environment: "dev" } }), log);
  assert.deepEqual(rsp.requirements?.resources[ENVIRONMENT_CONFIG]?.match, {
    case: "matchName",
    value: "dev",
  });
});

test("adds the mandatory tags without overwriting the team's", () => {
  const rsp = supplied({
    resources: {
      db: instance({
        region: "eu-central-1",
        instanceClass: "db.r6g.large",
        tags: { team: "orders", "data-classification": "confidential" },
      }),
    },
  });
  assert.deepEqual(forProvider(rsp, "db").tags, {
    team: "orders",
    "data-classification": "confidential",
    "cost-center": "4200",
  });
  assert.deepEqual(policyStatus(rsp), {
    environment: "prod",
    resources: 1,
    tagsAdded: 1,
    warnings: 0,
  });
  assert.deepEqual(
    rsp.results.map((r) => [r.severity, r.message]),
    [
      [
        Severity.NORMAL,
        "held 1 composed resource to environment prod's policy: 1 tag added, 0 warnings",
      ],
    ],
  );
  // The requirement still stands, and the context still passes on.
  assert.ok(rsp.requirements?.resources[ENVIRONMENT_CONFIG]);
  assert.deepEqual(rsp.context, { "example.org/previous-step": "kept" });
});

test("tags a resource that has none yet", () => {
  const rsp = supplied({ resources: { db: instance({}) } });
  assert.deepEqual(forProvider(rsp, "db").tags, PROD_POLICY.mandatoryTags);
});

test("a team tag overriding the environment's is kept, with a warning", () => {
  const rsp = supplied({
    resources: {
      db: instance({ tags: { "cost-center": "1234" } }),
      cache: instance({ tags: { "data-classification": "public" } }),
    },
  });
  assert.deepEqual(forProvider(rsp, "db").tags, {
    "cost-center": "1234",
    "data-classification": "confidential",
  });
  assert.deepEqual(forProvider(rsp, "cache").tags, {
    "data-classification": "public",
    "cost-center": "4200",
  });
  assert.deepEqual(policyStatus(rsp), {
    environment: "prod",
    resources: 2,
    tagsAdded: 2,
    warnings: 2,
  });
  assert.deepEqual(
    rsp.results.map((r) => [r.severity, r.message]),
    [
      [
        Severity.WARNING,
        `Instance "cache" keeps the team's tag data-classification=public over environment prod's data-classification=confidential`,
      ],
      [
        Severity.WARNING,
        `Instance "db" keeps the team's tag cost-center=1234 over environment prod's cost-center=4200`,
      ],
      [
        Severity.NORMAL,
        "held 2 composed resources to environment prod's policy: 2 tags added, 2 warnings",
      ],
    ],
  );
});

test("leaves resources that are not managed resources alone", () => {
  const configMap = { apiVersion: "v1", kind: "ConfigMap", data: { a: "b" } };
  const rsp = supplied({ resources: { settings: configMap } });
  assert.deepEqual(rsp.desired?.resources["settings"]?.resource, {
    apiVersion: "v1",
    kind: "ConfigMap",
    data: { a: "b" },
  });
  assert.deepEqual(policyStatus(rsp), {
    environment: "prod",
    resources: 0,
    tagsAdded: 0,
    warnings: 0,
  });
});

test("an environment without mandatory tags or allow lists judges nothing", () => {
  const rsp = runFunction(
    request({
      supplied: environmentConfig("prod", { policy: {} }),
      resources: { db: instance({ region: "ap-south-1" }) },
    }),
    log,
  );
  assert.deepEqual(forProvider(rsp, "db"), { region: "ap-south-1" });
});

test("keeps the status earlier steps desired", () => {
  const req = request({
    supplied: environmentConfig("prod", { policy: PROD_POLICY }),
  });
  req.desired!.composite = create(ResourceSchema, {
    resource: { status: { endpoint: "db.internal" } },
  });
  const rsp = runFunction(req, log);
  const xr = rsp.desired?.composite?.resource as JsonObject;
  assert.equal((xr.status as JsonObject).endpoint, "db.internal");
  assert.equal(
    ((xr.status as JsonObject).policy as JsonObject).environment,
    "prod",
  );
});

const prod = environmentConfig("prod", { policy: PROD_POLICY });

const fatal: Record<string, { call: Call; message: string }> = {
  "a region the environment does not allow": {
    call: {
      supplied: prod,
      resources: { db: instance({ region: "us-east-1" }) },
    },
    message:
      'Instance "db" sets spec.forProvider.region to "us-east-1", which environment prod does not allow (allowed: eu-central-1, eu-west-1)',
  },
  "an instance class the environment does not allow": {
    call: {
      supplied: prod,
      resources: { db: instance({ instanceClass: "db.t3.micro" }) },
    },
    message:
      'Instance "db" sets spec.forProvider.instanceClass to "db.t3.micro", which environment prod does not allow (allowed: db.r6g.large, db.r6g.xlarge)',
  },
  "every breach at once": {
    call: {
      supplied: prod,
      resources: {
        b: instance({ instanceClass: "db.t3.micro" }),
        a: instance({ region: "us-east-1" }),
      },
    },
    message:
      'Instance "a" sets spec.forProvider.region to "us-east-1", which environment prod does not allow (allowed: eu-central-1, eu-west-1); ' +
      'Instance "b" sets spec.forProvider.instanceClass to "db.t3.micro", which environment prod does not allow (allowed: db.r6g.large, db.r6g.xlarge)',
  },
  "tags that are not a map": {
    call: { supplied: prod, resources: { db: instance({ tags: ["a"] }) } },
    message:
      'Instance "db" sets spec.forProvider.tags to something other than a map',
  },
  "no EnvironmentConfig for the environment": {
    call: { supplied: null },
    message:
      "there is no EnvironmentConfig named prod, which holds the policy of environment prod",
  },
  "an EnvironmentConfig without a policy": {
    call: { supplied: environmentConfig("prod", { region: "eu-central-1" }) },
    message: "EnvironmentConfig prod has no data.policy",
  },
  "an allow list that is not a list of strings": {
    call: {
      supplied: environmentConfig("prod", {
        policy: { allowedRegions: "eu-central-1" },
      }),
    },
    message:
      "EnvironmentConfig prod: data.policy.allowedRegions must be a list of strings",
  },
  "mandatory tags that are not strings": {
    call: {
      supplied: environmentConfig("prod", {
        policy: { mandatoryTags: { "cost-center": 4200 } },
      }),
    },
    message:
      "EnvironmentConfig prod: data.policy.mandatoryTags must map tag names to strings",
  },
  "a composite resource without an environment": {
    call: { spec: { region: "eu-central-1" } },
    message:
      "the composite resource sets no spec.environment, which chooses the policy it is held to",
  },
  "a Crossplane that cannot supply required resources": {
    call: { capabilities: [Capability.CAPABILITIES, Capability.CONDITIONS] },
    message:
      "this Crossplane cannot supply required resources, which the policy gate reads its rules from",
  },
};

for (const [name, { call, message }] of Object.entries(fatal)) {
  test(`fatal: ${name}`, () => rejects(call, message));
}

test("a Crossplane that advertises required resources is asked", () => {
  const rsp = runFunction(
    request({
      capabilities: [Capability.CAPABILITIES, Capability.REQUIRED_RESOURCES],
    }),
    log,
  );
  assert.ok(rsp.requirements?.resources[ENVIRONMENT_CONFIG]);
});

test("handle carries the requirement over the wire", () => {
  const out = handle(toBinary(RunFunctionRequestSchema, request({})), log);
  const rsp = fromBinary(RunFunctionResponseSchema, out);
  assert.deepEqual(rsp.requirements?.resources[ENVIRONMENT_CONFIG]?.match, {
    case: "matchName",
    value: "prod",
  });
});

test("handle turns a breach into a fatal result", () => {
  const req = request({
    supplied: prod,
    resources: { db: instance({ region: "us-east-1" }) },
  });
  const out = handle(toBinary(RunFunctionRequestSchema, req), log);
  const rsp = fromBinary(RunFunctionResponseSchema, out);
  assert.equal(rsp.meta?.tag, "gate");
  assert.equal(rsp.desired, undefined);
  assert.deepEqual(
    rsp.results.map((r) => [r.severity, r.message]),
    [
      [
        Severity.FATAL,
        'Instance "db" sets spec.forProvider.region to "us-east-1", which environment prod does not allow (allowed: eu-central-1, eu-west-1)',
      ],
    ],
  );
});

test("handle reports a request it cannot decode", () => {
  const rsp = fromBinary(
    RunFunctionResponseSchema,
    handle(new Uint8Array([0xff]), log),
  );
  assert.equal(rsp.results[0]?.severity, Severity.FATAL);
  assert.match(
    rsp.results[0]?.message ?? "",
    /^cannot decode RunFunctionRequest: /,
  );
});
