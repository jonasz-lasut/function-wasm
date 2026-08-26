// Native tests for the guest's logic, under node --test (node strips the
// types): the fetch and log doubles stand in for the world's imports,
// exactly as the other guests' native tests stub their hosts.

import assert from "node:assert/strict";
import { test } from "node:test";

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import type { JsonObject } from "@bufbuild/protobuf";
import {
  RunFunctionRequestSchema,
  RunFunctionResponseSchema,
  Severity,
} from "../src/gen/run_function_pb.js";
import type {
  RunFunctionRequest,
  RunFunctionResponse,
} from "../src/gen/run_function_pb.js";
import { handle, runFunction } from "../src/fn.ts";

const log = () => {};

async function fakeFetch(url: string): Promise<string> {
  if (url === "https://greetings.example.com/en") {
    return "howdy";
  }
  throw new Error(
    `internal-error: sandbox.egress: no rule admits host "${url}"`,
  );
}

function xr(name: string) {
  return {
    composite: {
      resource: {
        apiVersion: "example.org/v1",
        kind: "XR",
        metadata: { name },
      },
    },
  };
}

function request(fields: object): RunFunctionRequest {
  return create(RunFunctionRequestSchema, {
    meta: { tag: "hello" },
    observed: xr("my-xr"),
    ...fields,
  });
}

function greetingOf(rsp: RunFunctionResponse): unknown {
  const resource = rsp.desired?.resources["greeting"]?.resource as JsonObject;
  return (resource.data as JsonObject).greeting;
}

test("default greeting", async () => {
  const rsp = await runFunction(request({}), fakeFetch, log);
  assert.equal(greetingOf(rsp), "hello my-xr");
  assert.equal(rsp.meta?.tag, "hello");
  assert.equal(rsp.results[0]?.message, "greeted my-xr");
  assert.equal(rsp.conditions[0]?.type, "FunctionSuccess");
});

test("configured greeting keeps desired", async () => {
  const rsp = await runFunction(
    request({
      input: { config: { greeting: "hi" } },
      desired: { resources: { other: {} } },
    }),
    fakeFetch,
    log,
  );
  assert.equal(greetingOf(rsp), "hi my-xr");
  assert.ok(rsp.desired?.resources["other"]);
});

test("bad config is an error", async () => {
  await assert.rejects(
    runFunction(
      request({ input: { config: { greeting: 7 } } }),
      fakeFetch,
      log,
    ),
    (e: unknown) => e === "cannot read config: greeting must be a string",
  );
});

test("greeting from url through the fetcher", async () => {
  const rsp = await runFunction(
    request({
      input: { config: { greetingUrl: "https://greetings.example.com/en" } },
    }),
    fakeFetch,
    log,
  );
  assert.equal(greetingOf(rsp), "howdy my-xr");
  await assert.rejects(
    runFunction(
      request({
        input: { config: { greetingUrl: "https://evil.example.com/en" } },
      }),
      fakeFetch,
      log,
    ),
    (e: unknown) =>
      e ===
      'cannot fetch greeting: internal-error: sandbox.egress: no rule admits host "https://evil.example.com/en"',
  );
});

test("handle round trip reports fatal", async () => {
  const req = create(RunFunctionRequestSchema, { meta: { tag: "t" } });
  const out = await handle(
    toBinary(RunFunctionRequestSchema, req),
    fakeFetch,
    log,
  );
  const rsp = fromBinary(RunFunctionResponseSchema, out);
  assert.equal(rsp.meta?.tag, "t");
  assert.equal(rsp.results[0]?.severity, Severity.FATAL);
  assert.equal(
    rsp.results[0]?.message,
    "cannot get observed composite resource: none in request",
  );
});
