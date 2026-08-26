// The hello-assemblyscript guest: a Crossplane composition function compiled
// to a wasm module and run by function-wasm. It composes a ConfigMap greeting
// the composite resource.
//
// runFunction is ordinary AssemblyScript over the messages as-proto-gen
// generated from the vendored crossplane proto (assembly/fnv1); the ABI
// exports and the wasmfn.log / wasmfn.http host imports live in abi.ts and
// host.ts.

import { Condition } from "./fnv1/apiextensions/fn/proto/v1/Condition";
import { Resource } from "./fnv1/apiextensions/fn/proto/v1/Resource";
import { ResponseMeta } from "./fnv1/apiextensions/fn/proto/v1/ResponseMeta";
import { Result } from "./fnv1/apiextensions/fn/proto/v1/Result";
import { RunFunctionRequest } from "./fnv1/apiextensions/fn/proto/v1/RunFunctionRequest";
import { RunFunctionResponse } from "./fnv1/apiextensions/fn/proto/v1/RunFunctionResponse";
import { Severity } from "./fnv1/apiextensions/fn/proto/v1/Severity";
import { State } from "./fnv1/apiextensions/fn/proto/v1/State";
import { Status } from "./fnv1/apiextensions/fn/proto/v1/Status";
import { Target } from "./fnv1/apiextensions/fn/proto/v1/Target";
import { Duration } from "./fnv1/google/protobuf/Duration";
import { Struct } from "./fnv1/google/protobuf/Struct";
import { Value } from "./fnv1/google/protobuf/Value";
import { getText } from "./http";
import { logInfo } from "./log";
import { Entry, field, object, stringValue, structValue } from "./structpb";

export const defaultTTLSeconds: i64 = 60;

// The outcome of runFunction: a response, or the message of a fatal result.
// AssemblyScript has no exceptions or union returns, so errors travel by value.
export class Outcome {
  rsp: RunFunctionResponse | null = null;
  err: string | null = null;

  static ok(rsp: RunFunctionResponse): Outcome {
    const o = new Outcome();
    o.rsp = rsp;
    return o;
  }

  static fail(err: string): Outcome {
    const o = new Outcome();
    o.err = err;
    return o;
  }
}

// A string read from the Input's config: absent, a value, or another kind.
class ConfigString {
  found: bool = false;
  bad: bool = false;
  value: string = "";
}

// configString reads a string field of the Input's config block.
function configString(req: RunFunctionRequest, key: string): ConfigString {
  const out = new ConfigString();
  const config = structValue(field(req.input, "config"));
  if (config === null) return out;
  const v = field(config, key);
  if (v === null) return out;
  const s = stringValue(v);
  if (s === null) {
    out.bad = true;
    return out;
  }
  out.found = true;
  out.value = s;
  return out;
}

function observedName(req: RunFunctionRequest): string | null {
  const observed = req.observed;
  if (observed === null) return null;
  const composite = observed.composite;
  if (composite === null) return null;
  const metadata = structValue(field(composite.resource, "metadata"));
  return stringValue(field(metadata, "name"));
}

// runFunction adds a ConfigMap greeting the composite resource to the desired
// state.
export function runFunction(req: RunFunctionRequest): Outcome {
  const meta = req.meta;
  const tag = meta === null ? "" : meta.tag;
  logInfo("Running function", ["tag", tag]);

  let greeting = "hello";
  const configured = configString(req, "greeting");
  if (configured.bad) {
    return Outcome.fail("cannot read config: greeting must be a string");
  }
  if (configured.found) {
    greeting = configured.value;
  }
  // greetingUrl fetches the greeting through the host instead - the
  // requires.egress grant of the module's manifest decides whether it may.
  const url = configString(req, "greetingUrl");
  if (url.bad) {
    return Outcome.fail("cannot read config: greetingUrl must be a string");
  }
  if (url.found) {
    const fetched = getText(url.value);
    const err = fetched.err;
    if (err !== null) {
      return Outcome.fail("cannot fetch greeting: " + err);
    }
    greeting = fetched.text;
  }

  const name = observedName(req);
  if (name === null) {
    return Outcome.fail("cannot get observed composite resource: none in request");
  }

  // The ConfigMap greeting the composite, added to the desired state the
  // request carried.
  const cm = object([
    new Entry("apiVersion", Value.ofString("v1")),
    new Entry("kind", Value.ofString("ConfigMap")),
    new Entry("data", Value.ofStruct(object([new Entry("greeting", Value.ofString(greeting + " " + name))]))),
  ]);
  const rsp = new RunFunctionResponse();
  rsp.meta = responseMeta(tag);
  const desiredIn = req.desired;
  const desired = desiredIn === null ? new State() : desiredIn;
  desired.resources.set("greeting", new Resource(cm));
  rsp.desired = desired;

  const result = new Result();
  result.severity = Severity.SEVERITY_NORMAL;
  result.message = "greeted " + name;
  result.hasTarget = true;
  result.target = Target.TARGET_COMPOSITE;
  rsp.results = [result];

  const condition = new Condition();
  condition.type = "FunctionSuccess";
  condition.status = Status.STATUS_CONDITION_TRUE;
  condition.reason = "Success";
  condition.hasTarget = true;
  condition.target = Target.TARGET_COMPOSITE_AND_CLAIM;
  rsp.conditions = [condition];

  return Outcome.ok(rsp);
}

export function responseMeta(tag: string): ResponseMeta {
  const meta = new ResponseMeta();
  meta.tag = tag;
  meta.ttl = new Duration(defaultTTLSeconds, 0);
  return meta;
}
