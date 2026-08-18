package codec

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/testing/protocmp"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
)

func TestConsumeRequest(t *testing.T) {
	cases := map[string]struct {
		reason string
		stash  bool
		want   bool
	}{
		"NoStash": {
			reason: "A request that was not stashed returns nil.",
			want:   false,
		},
		"Stashed": {
			reason: "A stashed request returns its bytes.",
			stash:  true,
			want:   true,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			req := &fnv1.RunFunctionRequest{Meta: &fnv1.RequestMeta{Tag: "test"}}
			if tc.stash {
				StashRequest(req, []byte{1, 2, 3})
			}
			got := ConsumeRequest(req)
			if tc.want && got == nil {
				t.Error(tc.reason + ": want non-nil raw bytes, got nil")
			}
			if !tc.want && got != nil {
				t.Errorf("%s: want nil, got %d bytes", tc.reason, len(got))
			}
		})
	}
}

func TestConsumeRequestTwice(t *testing.T) {
	req := &fnv1.RunFunctionRequest{}
	StashRequest(req, []byte{1, 2, 3})

	first := ConsumeRequest(req)
	if first == nil {
		t.Fatal("first ConsumeRequest should return non-nil")
	}
	second := ConsumeRequest(req)
	if second != nil {
		t.Fatal("second ConsumeRequest should return nil")
	}
}

func TestStashResponse(t *testing.T) {
	raw := []byte{4, 5, 6}
	rsp := &fnv1.RunFunctionResponse{}
	StashResponse(rsp, raw)
	got := consumeResponse(rsp)
	if diff := cmp.Diff(raw, got); diff != "" {
		t.Errorf("consumeResponse: -want, +got:\n%s", diff)
	}
	if again := consumeResponse(rsp); again != nil {
		t.Error("second consumeResponse should return nil")
	}
}

func TestRawCodecRoundTrip(t *testing.T) {
	req := &fnv1.RunFunctionRequest{
		Meta: &fnv1.RequestMeta{Tag: "test"},
	}
	wire, err := proto.Marshal(req)
	if err != nil {
		t.Fatal(err)
	}

	// Unmarshal through the codec: it should decode AND stash.
	c := rawCodec{}
	decoded := &fnv1.RunFunctionRequest{}
	if err := c.Unmarshal(wire, decoded); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(req, decoded, protocmp.Transform()); diff != "" {
		t.Errorf("decoded request: -want, +got:\n%s", diff)
	}
	stashed := ConsumeRequest(decoded)
	if diff := cmp.Diff(wire, stashed); diff != "" {
		t.Errorf("stashed bytes should match the wire input: -want, +got:\n%s", diff)
	}
}

func TestRawCodecMarshalStashedResponse(t *testing.T) {
	rsp := &fnv1.RunFunctionResponse{
		Meta: &fnv1.ResponseMeta{Tag: "test"},
	}
	raw := []byte{7, 8, 9}
	StashResponse(rsp, raw)

	c := rawCodec{}
	out, err := c.Marshal(rsp)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(raw, out); diff != "" {
		t.Errorf("Marshal should return the stashed bytes: -want, +got:\n%s", diff)
	}

	// Without stashed bytes, Marshal encodes normally.
	out2, err := c.Marshal(rsp)
	if err != nil {
		t.Fatal(err)
	}
	check := &fnv1.RunFunctionResponse{}
	if err := proto.Unmarshal(out2, check); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(rsp, check, protocmp.Transform()); diff != "" {
		t.Errorf("normal Marshal round-trip: -want, +got:\n%s", diff)
	}
}

func TestRawCodecNonRunFunctionFallsThrough(t *testing.T) {
	// A non-RunFunctionRequest/Response message uses the standard proto path.
	msg := &fnv1.RequestMeta{Tag: "test"}
	c := rawCodec{}
	wire, err := c.Marshal(msg)
	if err != nil {
		t.Fatal(err)
	}
	decoded := &fnv1.RequestMeta{}
	if err := c.Unmarshal(wire, decoded); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(msg, decoded, protocmp.Transform()); diff != "" {
		t.Errorf("non-RunFunction round-trip: -want, +got:\n%s", diff)
	}
}
