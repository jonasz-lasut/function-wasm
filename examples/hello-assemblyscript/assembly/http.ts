// HTTP egress through the host (docs/abi.md, "HTTP egress"): the guest never
// opens a socket; it hands the host a JSON request and the host performs it
// within the egress grant of the module's manifest and the operator's policy,
// or answers with a refusal. The response body is base64 both ways.

import { JSON } from "json-as";

import { hostHttp } from "./host";


@json
class WireRequest {
  method: string = "GET";
  url: string = "";
}


@json
class WireResponse {
  status: i32 = 0;
  body: string | null = null;
  error: string | null = null;
}

// The outcome of a request: text is the trimmed body of a 200, err the host's
// reason for anything else. AssemblyScript has no exceptions, so errors travel
// by value.
export class HttpText {
  text: string = "";
  err: string | null = null;
}

// getText GETs url through the host and returns the whitespace-trimmed body
// of a 200; any other answer (a refusal, a non-200 status, a payload this
// guest cannot read) is an err.
export function getText(url: string): HttpText {
  const out = new HttpText();
  const request = new WireRequest();
  request.url = url;
  const payload = String.UTF8.encode(JSON.stringify(request));
  const packed = hostHttp(changetype<usize>(payload), <u32>payload.byteLength);
  if (packed == 0) {
    out.err = "the host returned no response";
    return out;
  }
  const raw = String.UTF8.decodeUnsafe(<usize>(packed >>> 32), <usize>(packed & 0xffffffff));
  const response = JSON.parse<WireResponse>(raw);
  if (response.status == 0) {
    const reason = response.error;
    out.err = reason !== null && reason.length > 0 ? reason : "the host returned no status and no error";
    return out;
  }
  if (response.status != 200) {
    out.err = "GET " + url + ": status " + response.status.toString();
    return out;
  }
  const body = response.body;
  if (body === null) {
    return out;
  }
  const decoded = decodeBase64(body);
  if (decoded === null) {
    out.err = "the host's HTTP response body is not base64";
    return out;
  }
  out.text = decoded.trim();
  return out;
}

const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// decodeBase64 decodes the standard padded alphabet (how the host renders
// body bytes) into a string, or null when s is not base64.
function decodeBase64(s: string): string | null {
  const bytes = new Array<u8>();
  let acc: u32 = 0;
  let bits: i32 = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charAt(i);
    if (c == "=") break;
    const v = alphabet.indexOf(c);
    if (v < 0) return null;
    acc = (acc << 6) | <u32>v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      bytes.push(<u8>((acc >> bits) & 0xff));
    }
  }
  const buf = new ArrayBuffer(bytes.length);
  for (let i = 0; i < bytes.length; i++) {
    store<u8>(changetype<usize>(buf) + i, bytes[i]);
  }
  return String.UTF8.decode(buf);
}
