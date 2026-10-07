// The function through the codec, natively (moon test): a request encoded
// the way crossplane sends it - an input Struct, the observed composite, a
// packed capabilities list - decoded, run, and the response encoded and
// decoded again, so what is asserted is what the host would read. The host is
// a fake.

///|
fn[T : @protobuf.Write] encode(msg : T) -> Bytes raise {
  let buf = @buffer.Buffer()
  @protobuf.Write::write(msg, buf)
  buf.to_bytes()
}

///|
fn[T : @protobuf.Read] decode(bytes : Bytes) -> T raise {
  let reader = @protobuf.BytesReader::from_bytes(bytes) as &@protobuf.Reader
  @protobuf.Read::read(reader)
}

///|
fn text(s : String) -> @wkt.Value {
  @wkt.Value::Value(kind=@wkt.Value_Kind::StringValue(s))
}

///|
fn number(n : Double) -> @wkt.Value {
  @wkt.Value::Value(kind=@wkt.Value_Kind::NumberValue(n))
}

///|
fn object(fields : Map[String, @wkt.Value]) -> @wkt.Value {
  @wkt.Value::Value(
    kind=@wkt.Value_Kind::StructValue(@wkt.Struct::Struct(fields)),
  )
}

///|
/// request builds a RunFunctionRequest with the given config block, over the
/// wire and back, as the host hands it to the guest.
fn request(config : Map[String, @wkt.Value]) -> @fnv1.RunFunctionRequest raise {
  let input = @wkt.Struct::Struct({
    "apiVersion": text("wasm.fn.crossplane.io/v1"),
    "kind": text("Input"),
    "config": object(config),
  })
  let xr = @wkt.Struct::Struct({
    "apiVersion": text("example.org/v1"),
    "kind": text("XR"),
    "metadata": object({ "name": text("my-xr") }),
  })
  let req = @fnv1.RunFunctionRequest::RunFunctionRequest(
    Map([]),
    Map([]),
    Map([]),
    Map([]),
    meta=@fnv1.RequestMeta::RequestMeta("hello", [
      @fnv1.Capability::CAPABILITY_CAPABILITIES,
      @fnv1.Capability::CAPABILITY_CONDITIONS,
    ]),
    observed=@fnv1.State::State(
      Map([]),
      composite=@fnv1.Resource::Resource(
        Map([]),
        @fnv1.Ready::READY_UNSPECIFIED,
        resource=xr,
      ),
    ),
    input~,
  )
  decode(encode(req))
}

///|
/// run_through_codec runs the function on req with a fake host and returns
/// the response as decoded from its wire form, with the log lines captured.
fn run_through_codec(
  req : @fnv1.RunFunctionRequest,
  fetched : Result[String, String],
) -> (@fnv1.RunFunctionResponse, Array[String]) raise {
  let logs : Array[String] = []
  let host = @hello.Host::{
    info: (msg, kv) => {
      logs.push("\{msg} " + kv.map(pair => "\{pair.0}=\{pair.1}").join(" "))
    },
    get_text: _url => fetched,
  }
  let rsp = @fnv1.RunFunctionResponse::RunFunctionResponse([], [])
  @hello.run_function(host, req, rsp)
  (decode(encode(rsp)), logs)
}

///|
fn greeting_of(rsp : @fnv1.RunFunctionResponse) -> String? {
  guard rsp.desired is Some({ resources, .. }) else { return None }
  guard resources.get("greeting") is Some({ resource: Some(cm), .. }) else {
    return None
  }
  guard cm.fields.get("data")
    is Some({ kind: @wkt.Value_Kind::StructValue(data), }) else {
    return None
  }
  match data.fields.get("greeting") {
    Some({ kind: @wkt.Value_Kind::StringValue(s), }) => Some(s)
    _ => None
  }
}

///|
test "the default greeting" {
  let (rsp, logs) = run_through_codec(request(Map([])), Err("not asked"))
  assert_eq(greeting_of(rsp), Some("hello my-xr"))
  assert_eq(logs, ["Running function tag=hello"])
  assert_eq(rsp.results.length(), 1)
  assert_eq(rsp.results[0].message, "greeted my-xr")
  assert_true(rsp.results[0].severity == @fnv1.Severity::SEVERITY_NORMAL)
  assert_true(rsp.results[0].target == Some(@fnv1.Target::TARGET_COMPOSITE))
  assert_eq(rsp.conditions.length(), 1)
  assert_eq(rsp.conditions[0].type_, "FunctionSuccess")
  assert_eq(rsp.conditions[0].reason, "Success")
  assert_true(rsp.conditions[0].status == @fnv1.Status::STATUS_CONDITION_TRUE)
}

///|
test "the configured greeting" {
  let (rsp, _) = run_through_codec(
    request({ "greeting": text("hi") }),
    Err("not asked"),
  )
  assert_eq(greeting_of(rsp), Some("hi my-xr"))
}

///|
test "the greeting fetched through the host" {
  let (rsp, _) = run_through_codec(
    request({ "greetingUrl": text("http://greetings.example/en") }),
    Ok("howdy"),
  )
  assert_eq(greeting_of(rsp), Some("howdy my-xr"))
}

///|
test "a refused fetch is the function's error" {
  try {
    let _ = run_through_codec(
      request({ "greetingUrl": text("http://greetings.example/en") }),
      Err("sandbox.egress: HTTP egress is not granted to this module"),
    )
    fail("want FunctionError")
  } catch {
    @hello.FunctionError(msg) =>
      assert_eq(
        msg, "cannot fetch greeting: sandbox.egress: HTTP egress is not granted to this module",
      )
    e => raise e
  }
}

///|
test "a greeting that is not a string is the function's error" {
  try {
    let _ = run_through_codec(request({ "greeting": number(7) }), Err(""))
    fail("want FunctionError")
  } catch {
    @hello.FunctionError(msg) =>
      assert_eq(msg, "cannot read config: greeting must be a string")
    e => raise e
  }
}
