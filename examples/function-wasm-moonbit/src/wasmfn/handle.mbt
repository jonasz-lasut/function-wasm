// The function-wasm ABI v2 glue: the world's run behind the generated export
// (src/gen/world/function). A fresh component instance serves each request
// and the host drops it afterwards; the canonical ABI owns the request
// buffer the host lowered and the response buffer handed back (freed by the
// generated cabi_post_run once the host has copied it out).

///|
let default_ttl_seconds : Int64 = 60

///|
/// handle is one request: decode, run, encode. A request the codec cannot
/// decode, or a function error, becomes a fatal result on a response the
/// host can always read; the Err of run - the request's fatal result on the
/// host's side - is reached only when a reply cannot even be encoded.
pub fn handle(request : FixedArray[Byte]) -> Result[FixedArray[Byte], String] {
  let reader = @protobuf.BytesReader::from_bytes(Bytes::from_array(request[:]))
    as &@protobuf.Reader
  let req : @fnv1.RunFunctionRequest = @protobuf.Read::read(reader) catch {
    e => return encode(fatal("", "cannot decode RunFunctionRequest: \{e}"))
  }
  let tag = match req.meta {
    Some(meta) => meta.tag
    None => ""
  }
  let rsp = @fnv1.RunFunctionResponse::RunFunctionResponse(
    [],
    [],
    meta=meta(tag),
  )
  @hello.run_function(host(), req, rsp) catch {
    @hello.FunctionError(msg) => return encode(fatal(tag, msg))
  }
  encode(rsp)
}

///|
/// The host as the function sees it: the typed log import and a GET over
/// wasi:http@0.2 (http.mbt).
fn host() -> @hello.Host {
  { info, get_text, }
}

///|
fn meta(tag : String) -> @fnv1.ResponseMeta {
  @fnv1.ResponseMeta::ResponseMeta(
    tag,
    ttl=@wkt.Duration::Duration(default_ttl_seconds, 0),
  )
}

///|
/// fatal is a fresh response carrying one fatal result.
fn fatal(tag : String, msg : String) -> @fnv1.RunFunctionResponse {
  @fnv1.RunFunctionResponse::RunFunctionResponse(
    [
      @fnv1.Result::Result(
        @fnv1.Severity::SEVERITY_FATAL,
        msg,
        target=@fnv1.Target::TARGET_COMPOSITE,
      ),
    ],
    [],
    meta=meta(tag),
  )
}

///|
fn encode(rsp : @fnv1.RunFunctionResponse) -> Result[FixedArray[Byte], String] {
  let buf = @buffer.Buffer()
  @protobuf.Write::write(rsp, buf) catch {
    e => return Err("cannot encode RunFunctionResponse: \{e}")
  }
  Ok(buf.to_bytes().to_fixedarray())
}
