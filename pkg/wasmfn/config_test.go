package wasmfn

import (
	"testing"

	"github.com/google/go-cmp/cmp"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
)

func TestGetConfig(t *testing.T) {
	type config struct {
		Replicas int    `json:"replicas"`
		Name     string `json:"name"`
	}
	type want struct {
		cfg config
		ok  bool
		err string
	}
	cases := map[string]struct {
		reason string
		input  string
		want   want
	}{
		"Present": {
			reason: "The config object decodes into the guest's struct.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"path":"x"},"config":{"replicas":3,"name":"web"}}`,
			want:   want{cfg: config{Replicas: 3, Name: "web"}, ok: true},
		},
		"Absent": {
			reason: "Without a config field nothing is decoded and ok is false.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"path":"x"}}`,
			want:   want{ok: false},
		},
		"WrongShape": {
			reason: "A config that does not fit the struct is an error.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","config":{"replicas":"many"}}`,
			want:   want{ok: true, err: "cannot decode input.config"},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			req := &fnv1.RunFunctionRequest{Input: resource.MustStructJSON(tc.input)}
			var cfg config
			ok, err := GetConfig(req, &cfg)
			if tc.want.err != "" {
				if err == nil || !cmp.Equal(true, contains(err.Error(), tc.want.err)) {
					t.Fatalf("\n%s\nGetConfig(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
			} else if err != nil {
				t.Fatalf("\n%s\nGetConfig(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.ok, ok); diff != "" {
				t.Errorf("\n%s\nGetConfig() ok: -want, +got:\n%s", tc.reason, diff)
			}
			if tc.want.err == "" {
				if diff := cmp.Diff(tc.want.cfg, cfg); diff != "" {
					t.Errorf("\n%s\nGetConfig() cfg: -want, +got:\n%s", tc.reason, diff)
				}
			}
		})
	}
}

func contains(s, sub string) bool {
	return len(sub) == 0 || (len(s) >= len(sub) && (s == sub || indexOf(s, sub) >= 0))
}

func indexOf(s, sub string) int {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return i
		}
	}
	return -1
}
