package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	"github.com/google/go-containerregistry/pkg/registry"
	"github.com/google/go-containerregistry/pkg/v1/empty"
	"github.com/google/go-containerregistry/pkg/v1/mutate"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/static"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

const ttl = 60 * time.Second

func digestOf(b []byte) string {
	sum := sha256.Sum256(b)
	return "sha256:" + hex.EncodeToString(sum[:])
}

// guestResponse is what the test guests return: desired resources and a
// result, so the diff proves the host forwards everything.
func guestResponse() *fnv1.RunFunctionResponse {
	rsp := &fnv1.RunFunctionResponse{
		Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)},
		Desired: &fnv1.State{Resources: map[string]*fnv1.Resource{
			"cm": {Resource: resource.MustStructJSON(`{"apiVersion":"v1","kind":"ConfigMap","data":{"xr":"my-xr"}}`)},
		}},
	}
	response.Normal(rsp, "hello from wasm")
	return rsp
}

func input(t *testing.T, module map[string]any) *structpb.Struct {
	t.Helper()
	in, err := structpb.NewStruct(map[string]any{
		"apiVersion": "wasm.fn.crossplane.io/v1beta1",
		"kind":       "Input",
		"module":     module,
	})
	if err != nil {
		t.Fatal(err)
	}
	return in
}

func fatal(msg string) *fnv1.RunFunctionResponse {
	return &fnv1.RunFunctionResponse{
		Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)},
		Results: []*fnv1.Result{{
			Severity: fnv1.Severity_SEVERITY_FATAL,
			Message:  msg,
			Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
		}},
	}
}

// privateRegistry serves an in-memory registry that requires basic auth for
// everything but the version check.
func privateRegistry(t *testing.T) (host string) {
	t.Helper()
	handler := registry.New()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		if req.URL.Path != "/v2/" {
			user, pass, ok := req.BasicAuth()
			if !ok || user != "robot" || pass != "s3cret" {
				w.Header().Set("WWW-Authenticate", `Basic realm="test"`)
				http.Error(w, "unauthorized", http.StatusUnauthorized)
				return
			}
		}
		handler.ServeHTTP(w, req)
	}))
	t.Cleanup(srv.Close)
	return strings.TrimPrefix(srv.URL, "http://")
}

func push(t *testing.T, ref string, wasm []byte) {
	t.Helper()
	img, err := mutate.AppendLayers(empty.Image, static.NewLayer(wasm, "application/wasm"))
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := name.ParseReference(ref)
	if err != nil {
		t.Fatal(err)
	}
	if err := remote.Write(parsed, img, remote.WithAuth(&authn.Basic{Username: "robot", Password: "s3cret"})); err != nil {
		t.Fatalf("cannot push %s: %v", ref, err)
	}
}

func TestRunFunction(t *testing.T) {
	okModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	emptyModule := testwasm.Fixed(t, &fnv1.RunFunctionResponse{}, testwasm.Options{Body: "(i64.const 0)"})
	trapModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{Body: "unreachable"})

	moduleDir := t.TempDir()
	for name, wasm := range map[string][]byte{"fn.wasm": okModule, "empty.wasm": emptyModule, "trap.wasm": trapModule, "notwasm.wasm": []byte("hello")} {
		if err := os.WriteFile(filepath.Join(moduleDir, name), wasm, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	registryHost := privateRegistry(t)
	push(t, registryHost+"/fn:v1", okModule)

	// An XR whose status names modules, for the *From sources.
	xr := resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"},"spec":{"module":"fn.wasm"},"status":{"module":{"ref":"` + registryHost + `/fn:v1","credentials":"registry"}}}`)
	credentials := map[string]*fnv1.Credentials{
		"registry": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
			Data: map[string][]byte{"username": []byte("robot"), "password": []byte("s3cret")},
		}}},
	}

	type args struct {
		req *fnv1.RunFunctionRequest
	}
	type want struct {
		rsp *fnv1.RunFunctionResponse
		err error
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"InvalidInput": {
			reason: "An input that is not an Input is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: resource.MustStructJSON(`{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":"nope"}`),
			}},
			want: want{rsp: fatal(`cannot get function input from *v1.RunFunctionRequest: cannot get function input *v1beta1.Input from *v1.RunFunctionRequest: cannot unmarshal JSON from *structpb.Struct into *v1beta1.Input: json: cannot unmarshal JSON string into Go v1beta1.ModuleSource within "/module"`)},
		},
		"NoSource": {
			reason: "A module without a source is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{}),
			}},
			want: want{rsp: fatal("cannot resolve module: module must set exactly one of oci, http, path, ociFrom, httpFrom and pathFrom")},
		},
		"PathModule": {
			reason: "A module served from the module directory runs and its response is returned verbatim.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"path": "fn.wasm"}),
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIModuleWithCredentials": {
			reason: "An OCI module is pulled with the pipeline step's credentials.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       input(t, map[string]any{"oci": map[string]any{"ref": registryHost + "/fn:v1", "credentials": "registry"}}),
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIModuleMissingCredentials": {
			reason: "Naming a credential the step does not carry is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"oci": map[string]any{"ref": registryHost + "/fn:v1", "credentials": "registry"}}),
			}},
			want: want{rsp: fatal(`cannot get credentials "registry" for module.oci: registry: credential not found`)},
		},
		"OCIFromStatus": {
			reason: "ociFrom reads the OCI source, credentials name included, from the observed XR.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       input(t, map[string]any{"ociFrom": "status.module"}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"PathFromSpec": {
			reason: "pathFrom reads the module path from the observed XR.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"pathFrom": "spec.module"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: guestResponse()},
		},
		"FromMissingField": {
			reason: "A *From field the XR lacks is a fatal result naming it.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"ociFrom": "status.other"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal("cannot resolve module: module.ociFrom: cannot read status.other from the composite resource: status.other: no such field")},
		},
		"FromWithoutComposite": {
			reason: "A *From source without an observed XR is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"pathFrom": "spec.module"}),
			}},
			want: want{rsp: fatal("cannot resolve module: module.pathFrom spec.module: no observed composite resource to read it from")},
		},
		"MetaFilledForEmptyResponse": {
			reason: "A guest that returns nothing still yields a well-formed response.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"path": "empty.wasm"}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)}}},
		},
		"GuestTrap": {
			reason: "A trapping guest is a fatal result naming the module.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"path": "trap.wasm"}),
			}},
			want: want{rsp: fatal("module module file trap.wasm failed: wasmfn_run failed: trap: unreachable code reached (a Go guest prints the panic to stderr)")},
		},
		"NotAModule": {
			reason: "Bytes that do not compile are a fatal result at load time.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"path": "notwasm.wasm"}),
			}},
			want: want{rsp: fatal("cannot load module module file notwasm.wasm: cannot compile module: failed to parse WebAssembly module")},
		},
	}

	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{Dir: moduleDir, Keychain: authn.NewMultiKeychain()})
	if err != nil {
		t.Fatal(err)
	}
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(4), resolver: resolver}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			rsp, err := f.RunFunction(context.Background(), tc.args.req)

			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.err, err, cmpopts.EquateErrors()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want err, +got err:\n%s", tc.reason, diff)
			}
		})
	}
}
