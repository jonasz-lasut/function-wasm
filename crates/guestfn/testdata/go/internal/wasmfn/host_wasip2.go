//go:build wasip1

// The world's wiring (wit/world.wit): the run export, the typed log import
// and wasi:http@0.2 behind HTTPClient, over the bindings wit-bindgen's Go
// generator wrote under internal/bindings. Built for GOOS=wasip1 - mainline
// Go has no wasip2 port - and lifted into the WASI 0.2 world by the adapter
// guestfn build links in.

package wasmfn

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"

	witTypes "go.bytecodealliance.org/pkg/wit/types"

	exports "github.com/example/my-fn/internal/bindings/export_wit_world"
	handler "github.com/example/my-fn/internal/bindings/wasi_http_outgoing_handler"
	types "github.com/example/my-fn/internal/bindings/wasi_http_types"
	streams "github.com/example/my-fn/internal/bindings/wasi_io_streams"
	_ "github.com/example/my-fn/internal/bindings/wit_exports"
	"github.com/example/my-fn/internal/bindings/wit_world"
)

// The generated wit_exports package calls export_wit_world.Run for the
// world's run export; filling the slot here, from the glue's own init, keeps
// that package import-free and the import graph free of a cycle.
func init() {
	exports.Run = runExport
}

// runExport is the world's run export: RunFunctionRequest bytes in,
// RunFunctionResponse bytes out. Every failure is encoded into the response
// as a fatal result, so the error branch of the world's result is never
// taken - the host can always decode what comes back.
func runExport(request []byte) witTypes.Result[[]byte, string] {
	return witTypes.Ok[[]byte, string](handle(context.Background(), request))
}

func hostLogSink(level Level, msg string, pairs [][2]string) {
	kv := make([]witTypes.Tuple2[string, string], len(pairs))
	for i, p := range pairs {
		kv[i] = witTypes.Tuple2[string, string]{F0: p[0], F1: p[1]}
	}
	wit_world.Log(wit_world.LogLevel(level), msg, kv)
}

// hostHTTPCall performs one request through wasi:http@0.2's
// outgoing-handler: an outgoing-request resource, the body written and
// finished, handle, then the future-incoming-response polled to completion
// through wasi:io's pollable (a blocking host call, which a sync-lifted
// guest may make) and the incoming body read to its end.
func hostHTTPCall(req hostRequest) (*hostResponse, error) {
	entries := make([]witTypes.Tuple2[string, []byte], 0, len(req.Headers))
	for k, vs := range req.Headers {
		for _, v := range vs {
			entries = append(entries, witTypes.Tuple2[string, []byte]{F0: k, F1: []byte(v)})
		}
	}
	headers := types.FieldsFromList(entries)
	if headers.IsErr() {
		return nil, fmt.Errorf("wasmfn: cannot set request headers: header-error %d", headers.Err().Tag())
	}
	oreq := types.MakeOutgoingRequest(headers.Ok())
	if oreq.SetMethod(method(req.Method)).IsErr() {
		return nil, fmt.Errorf("wasmfn: invalid method %q", req.Method)
	}
	if oreq.SetScheme(witTypes.Some(scheme(req.URL.Scheme))).IsErr() {
		return nil, fmt.Errorf("wasmfn: invalid scheme %q", req.URL.Scheme)
	}
	if oreq.SetAuthority(witTypes.Some(req.URL.Host)).IsErr() {
		return nil, fmt.Errorf("wasmfn: invalid authority %q", req.URL.Host)
	}
	if oreq.SetPathWithQuery(witTypes.Some(pathWithQuery(req.URL))).IsErr() {
		return nil, fmt.Errorf("wasmfn: invalid path %q", req.URL.RequestURI())
	}
	if len(req.Body) > 0 {
		if err := writeBody(oreq, req.Body); err != nil {
			return nil, err
		}
	}

	future := handler.Handle(oreq, witTypes.None[*types.RequestOptions]())
	if future.IsErr() {
		return nil, httpError(future.Err())
	}
	f := future.Ok()
	defer f.Drop()
	var got witTypes.Result[witTypes.Result[*types.IncomingResponse, types.ErrorCode], witTypes.Unit]
	for {
		if o := f.Get(); o.IsSome() {
			got = o.Some()
			break
		}
		p := f.Subscribe()
		p.Block()
		p.Drop()
	}
	if got.IsErr() {
		return nil, errors.New("wasmfn: the response was already taken")
	}
	if got.Ok().IsErr() {
		return nil, httpError(got.Ok().Err())
	}
	rsp := got.Ok().Ok()
	defer rsp.Drop()

	out := &hostResponse{Status: int(rsp.Status())}
	h := rsp.Headers()
	for _, e := range h.Entries() {
		out.Headers = append(out.Headers, [2]string{e.F0, string(e.F1)})
	}
	h.Drop()
	body, err := readBody(rsp)
	if err != nil {
		return nil, err
	}
	out.Body = body
	return out, nil
}

// writeBody streams a request body in wasi:io's chunks and finishes the
// outgoing-body, which wasi:http requires before the request is handled.
func writeBody(oreq *types.OutgoingRequest, body []byte) error {
	ob := oreq.Body()
	if ob.IsErr() {
		return errors.New("wasmfn: request body already taken")
	}
	stream := ob.Ok().Write()
	if stream.IsErr() {
		return errors.New("wasmfn: request body stream already taken")
	}
	out := stream.Ok()
	for off := 0; off < len(body); {
		n := min(4096, len(body)-off)
		if r := out.BlockingWriteAndFlush(body[off : off+n]); r.IsErr() {
			out.Drop()
			return errors.New("wasmfn: cannot write request body: " + streamError(r.Err()))
		}
		off += n
	}
	out.Drop()
	if r := types.OutgoingBodyFinish(ob.Ok(), witTypes.None[*types.Fields]()); r.IsErr() {
		return httpError(r.Err())
	}
	return nil
}

// readBody consumes the incoming body whole.
func readBody(rsp *types.IncomingResponse) ([]byte, error) {
	body := rsp.Consume()
	if body.IsErr() {
		return nil, errors.New("wasmfn: response body already taken")
	}
	stream := body.Ok().Stream()
	if stream.IsErr() {
		return nil, errors.New("wasmfn: response body stream already taken")
	}
	in := stream.Ok()
	var out []byte
	for {
		chunk := in.BlockingRead(64 << 10)
		if chunk.IsErr() {
			if chunk.Err().Tag() == streams.StreamErrorClosed {
				break
			}
			in.Drop()
			return nil, errors.New("wasmfn: cannot read response body: " + streamError(chunk.Err()))
		}
		out = append(out, chunk.Ok()...)
	}
	in.Drop()
	types.IncomingBodyFinish(body.Ok()).Drop()
	return out, nil
}

// pathWithQuery is the request-target wasi:http takes: the path, the query
// when there is one, never empty.
func pathWithQuery(u *url.URL) string {
	target := u.RequestURI()
	if target == "" || strings.HasPrefix(target, "?") {
		target = "/" + target
	}
	return target
}

func method(m string) types.Method {
	switch m {
	case "", http.MethodGet:
		return types.MakeMethodGet()
	case http.MethodHead:
		return types.MakeMethodHead()
	case http.MethodPost:
		return types.MakeMethodPost()
	case http.MethodPut:
		return types.MakeMethodPut()
	case http.MethodDelete:
		return types.MakeMethodDelete()
	case http.MethodConnect:
		return types.MakeMethodConnect()
	case http.MethodOptions:
		return types.MakeMethodOptions()
	case http.MethodTrace:
		return types.MakeMethodTrace()
	case http.MethodPatch:
		return types.MakeMethodPatch()
	}
	return types.MakeMethodOther(m)
}

func scheme(s string) types.Scheme {
	switch s {
	case "http":
		return types.MakeSchemeHttp()
	case "https":
		return types.MakeSchemeHttps()
	}
	return types.MakeSchemeOther(s)
}

func streamError(e streams.StreamError) string {
	if e.Tag() == streams.StreamErrorLastOperationFailed {
		err := e.LastOperationFailed()
		defer err.Drop()
		return err.ToDebugString()
	}
	return "stream closed"
}

// httpError turns a wasi:http error-code into the error HTTPClient's callers
// see: an *HTTPError carrying the host's reason. The runtime reports every
// failure of its own - a refused grant, a blocked address, a budget, a
// transport error - as internal-error carrying exactly the string ABI v1
// put in the wire's error field; any other code is named by its number.
func httpError(code types.ErrorCode) error {
	if code.Tag() == types.ErrorCodeInternalError {
		if reason := code.InternalError(); reason.IsSome() {
			return &HTTPError{Reason: reason.Some()}
		}
	}
	return &HTTPError{Reason: fmt.Sprintf("wasi:http error-code %d", code.Tag())}
}
