// HTTP through the host over wasi:http@0.2's outgoing-handler (the bindings
// under src/interface, wit-bindgen's): the runtime serves it through its
// egress policy - the three-layer grant decides whether a send is backed or
// refused, and a refusal reaches the guest as the internal-error code
// carrying the runtime's reason, never a trap. Sync: the future is blocked on
// through its pollable, which is what a sync-lifted run may do.

///|
let read_chunk : UInt64 = 65536

///|
/// get_text performs a GET on url and answers the body text of a 200,
/// trimmed; anything else is the reason as a string.
pub fn get_text(url : String) -> Result[String, String] {
  let (scheme, authority, path) = match split_url(url) {
    Ok(parts) => parts
    Err(e) => return Err(e)
  }
  let request = @types.OutgoingRequest::outgoing_request(
    @types.Fields::fields(),
  )
  guard request.set_method(@types.Method::Get) is Ok(_) else {
    return Err("the host refused the request method")
  }
  guard request.set_scheme(Some(scheme)) is Ok(_) &&
    request.set_authority(Some(authority)) is Ok(_) &&
    request.set_path_with_query(Some(path)) is Ok(_) else {
    return Err("the host refused the URL")
  }
  let future = match @outgoing_handler.handle(request, None) {
    Ok(future) => future
    Err(code) => return Err(describe(code))
  }
  let ready = future.subscribe()
  ready.block()
  ready.drop()
  let response = match future.get() {
    None => return Err("the host answered nothing")
    Some(Err(_)) => return Err("the response was already taken")
    Some(Ok(Err(code))) => return Err(describe(code))
    Some(Ok(Ok(response))) => response
  }
  future.drop()
  let status = response.status()
  if status != 200 {
    response.drop()
    return Err("GET \{url}: status \{status}")
  }
  let body = match response.consume() {
    Ok(body) => body
    Err(_) => return Err("the response body was already taken")
  }
  let stream = match body.stream() {
    Ok(stream) => stream
    Err(_) => return Err("the response body stream was already taken")
  }
  let buf = @buffer.Buffer()
  let read = drain(stream, buf)
  stream.drop()
  @types.IncomingBody::finish(body).drop()
  response.drop()
  match read {
    Ok(_) => Ok(@utf8.decode_lossy(buf.to_bytes()[:]).trim().to_owned())
    Err(e) => Err(e)
  }
}

///|
/// drain reads the response body stream to its end into buf.
fn drain(
  stream : @streams.InputStream,
  buf : @buffer.Buffer,
) -> Result[Unit, String] {
  while true {
    match stream.blocking_read(read_chunk) {
      Ok(chunk) => buf.write_bytes(Bytes::from_array(chunk[:])[:])
      Err(@streams.StreamError::Closed) => break
      Err(_) => return Err("reading the response body failed")
    }
  }
  Ok(())
}

///|
/// describe words a wasi:http error-code. internal-error carries the host's
/// own reason - the refusal of the egress grant or policy, a budget, a
/// transport failure - verbatim; every other case is named.
fn describe(code : @types.ErrorCode) -> String {
  match code {
    @types.ErrorCode::InternalError(Some(reason)) => reason
    other => "wasi:http error-code \{other}"
  }
}

///|
fn split_url(url : String) -> Result[(@types.Scheme, String, String), String] {
  let (scheme, rest) = if url.has_prefix("https://") {
    (@types.Scheme::Https, url[8:].to_owned())
  } else if url.has_prefix("http://") {
    (@types.Scheme::Http, url[7:].to_owned())
  } else {
    return Err("only http and https URLs work")
  }
  let (authority, path) = match rest.find("/") {
    Some(slash) => (rest[:slash].to_owned(), rest[slash:].to_owned())
    None => (rest, "/")
  }
  if authority == "" {
    return Err("the URL has no host")
  }
  Ok((scheme, authority, path))
}
