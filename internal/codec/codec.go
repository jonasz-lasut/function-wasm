// Package codec registers a gRPC codec that stashes the raw wire bytes of
// RunFunctionRequest messages so the host can forward them to a guest module
// without re-marshaling, and returns stashed raw bytes of RunFunctionResponse
// messages to gRPC without re-marshaling.
//
// The codec replaces the default "proto" codec via encoding.RegisterCodec in
// an init function: acceptable in a process with one gRPC server. Every
// non-RunFunctionRequest/Response message (health checks, reflection) falls
// through to the standard proto path.
package codec

import (
	"runtime"
	"slices"
	"sync"
	"weak"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"google.golang.org/grpc/encoding"
	"google.golang.org/protobuf/proto"
)

func init() {
	encoding.RegisterCodec(rawCodec{})
}

// rawCodec wraps the standard proto codec: it decodes every message normally
// and, for RunFunctionRequest, stashes a clone of the wire bytes so the host
// can forward them to a guest without re-encoding. For RunFunctionResponse,
// if raw bytes have been stashed by the caller, Marshal returns them instead
// of re-encoding.
type rawCodec struct{}

func (rawCodec) Name() string { return "proto" }

func (rawCodec) Marshal(v any) ([]byte, error) {
	if rsp, ok := v.(*fnv1.RunFunctionResponse); ok {
		if raw := consumeResponse(rsp); raw != nil {
			return raw, nil
		}
	}
	return proto.Marshal(v.(proto.Message))
}

func (rawCodec) Unmarshal(data []byte, v any) error {
	if err := proto.Unmarshal(data, v.(proto.Message)); err != nil {
		return err
	}
	if req, ok := v.(*fnv1.RunFunctionRequest); ok {
		StashRequest(req, slices.Clone(data))
	}
	return nil
}

var (
	reqStore sync.Map // weak.Pointer[fnv1.RunFunctionRequest] -> []byte
	rspStore sync.Map // weak.Pointer[fnv1.RunFunctionResponse] -> []byte
)

// StashRequest stashes raw wire bytes alongside a RunFunctionRequest.
// Normally called by the codec's Unmarshal; exposed so tests can exercise
// the raw path without going through gRPC.
func StashRequest(req *fnv1.RunFunctionRequest, data []byte) {
	wp := weak.Make(req)
	reqStore.Store(wp, data)
	runtime.AddCleanup(req, func(wp weak.Pointer[fnv1.RunFunctionRequest]) {
		reqStore.Delete(wp)
	}, wp)
}

// ConsumeRequest retrieves and removes the stashed raw bytes of req.
// It returns nil when no bytes were stashed (the request did not arrive
// through the codec, or was already consumed).
func ConsumeRequest(req *fnv1.RunFunctionRequest) []byte {
	wp := weak.Make(req)
	v, ok := reqStore.LoadAndDelete(wp)
	if !ok {
		return nil
	}
	return v.([]byte)
}

// StashResponse stashes raw response bytes so the codec's Marshal returns
// them instead of re-encoding. The caller must ensure the bytes are a valid
// encoding of rsp.
func StashResponse(rsp *fnv1.RunFunctionResponse, data []byte) {
	wp := weak.Make(rsp)
	rspStore.Store(wp, data)
	runtime.AddCleanup(rsp, func(wp weak.Pointer[fnv1.RunFunctionResponse]) {
		rspStore.Delete(wp)
	}, wp)
}

func consumeResponse(rsp *fnv1.RunFunctionResponse) []byte {
	wp := weak.Make(rsp)
	v, ok := rspStore.LoadAndDelete(wp)
	if !ok {
		return nil
	}
	return v.([]byte)
}
