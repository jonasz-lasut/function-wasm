// The hello-moonbit guest: a Crossplane composition function in MoonBit,
// compiled by moon into the core module guestfn build wraps into an ABI v2
// component, run by function-wasm. It composes a ConfigMap greeting the
// composite resource.
//
// run_function is ordinary MoonBit over the structs protoc-gen-mbt generated
// from the vendored crossplane proto (src/fnv1) and the google.protobuf types
// the moonbitlang/protobuf runtime ships; the world's run export and the log /
// wasi:http host imports live in src/wasmfn, behind the bindings wit-bindgen
// generated from wit/. Nothing here touches a host import: the host arrives
// as two functions, so moon test runs this package with fakes.

///|
/// What the function needs from its host: the runtime's log at info level,
/// and an HTTP GET through the host's egress answering the trimmed body text
/// of a 200.
pub(all) struct Host {
  info : (String, Array[(String, String)]) -> Unit
  get_text : (String) -> Result[String, String]
}

///|
/// A failure of the function: the glue turns it into the request's fatal
/// result.
pub(all) suberror FunctionError {
  FunctionError(String)
}

///|
priv enum ConfigString {
  Absent
  Text(String)
  NotText
}

///|
/// config_string reads a string field of the Input's config block.
fn config_string(req : @fnv1.RunFunctionRequest, key : String) -> ConfigString {
  guard req.input is Some(input) else { return Absent }
  guard input.fields.get("config")
    is Some({ kind: @wkt.Value_Kind::StructValue(config), }) else {
    return Absent
  }
  match config.fields.get(key) {
    Some({ kind: @wkt.Value_Kind::StringValue(s), }) => Text(s)
    Some(_) => NotText
    None => Absent
  }
}

///|
fn observed_name(req : @fnv1.RunFunctionRequest) -> String? {
  guard req.observed
    is Some({ composite: Some({ resource: Some(xr), .. }), .. }) else {
    return None
  }
  guard xr.fields.get("metadata")
    is Some({ kind: @wkt.Value_Kind::StructValue(metadata), }) else {
    return None
  }
  match metadata.fields.get("name") {
    Some({ kind: @wkt.Value_Kind::StringValue(name), }) => Some(name)
    _ => None
  }
}

///|
fn string_value(s : String) -> @wkt.Value {
  @wkt.Value::Value(kind=@wkt.Value_Kind::StringValue(s))
}

///|
fn struct_value(fields : Map[String, @wkt.Value]) -> @wkt.Value {
  @wkt.Value::Value(
    kind=@wkt.Value_Kind::StructValue(@wkt.Struct::Struct(fields)),
  )
}

///|
/// run_function fills rsp for req: the greeting ConfigMap added to the
/// desired state the request carried, a normal result and a success
/// condition. It raises for what it cannot do.
pub fn run_function(
  host : Host,
  req : @fnv1.RunFunctionRequest,
  rsp : @fnv1.RunFunctionResponse,
) -> Unit raise FunctionError {
  let tag = match req.meta {
    Some(meta) => meta.tag
    None => ""
  }
  (host.info)("Running function", [("tag", tag)])
  let mut greeting = "hello"
  match config_string(req, "greeting") {
    Text(s) => greeting = s
    NotText =>
      raise FunctionError("cannot read config: greeting must be a string")
    Absent => ()
  }
  // greetingUrl fetches the greeting through the host instead - the
  // requires.egress grant of the module's manifest decides whether it may.
  match config_string(req, "greetingUrl") {
    NotText =>
      raise FunctionError("cannot read config: greetingUrl must be a string")
    Text(url) =>
      match (host.get_text)(url) {
        Ok(text) => greeting = text
        Err(e) => raise FunctionError("cannot fetch greeting: \{e}")
      }
    Absent => ()
  }
  guard observed_name(req) is Some(name) else {
    raise FunctionError(
      "cannot get observed composite resource: none in request",
    )
  }

  // The ConfigMap greeting the composite, added to the desired state the
  // request carried - in a new map, so the request's own is never modified.
  let cm = @wkt.Struct::Struct({
    "apiVersion": string_value("v1"),
    "kind": string_value("ConfigMap"),
    "data": struct_value({ "greeting": string_value("\{greeting} \{name}") }),
  })
  let resources : Map[String, @fnv1.Resource] = Map([])
  let composite = match req.desired {
    Some(desired) => {
      for key, resource in desired.resources {
        resources[key] = resource
      }
      desired.composite
    }
    None => None
  }
  resources["greeting"] = @fnv1.Resource::Resource(
    {},
    @fnv1.Ready::READY_UNSPECIFIED,
    resource=cm,
  )
  rsp.desired = Some(@fnv1.State::State(resources, composite?))
  rsp.results = [
    @fnv1.Result::Result(
      @fnv1.Severity::SEVERITY_NORMAL,
      "greeted \{name}",
      target=@fnv1.Target::TARGET_COMPOSITE,
    ),
  ]
  rsp.conditions = [
    @fnv1.Condition::Condition(
      "FunctionSuccess",
      @fnv1.Status::STATUS_CONDITION_TRUE,
      "Success",
      target=@fnv1.Target::TARGET_COMPOSITE_AND_CLAIM,
    ),
  ]
}
