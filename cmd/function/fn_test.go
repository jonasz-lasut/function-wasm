package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"encoding/pem"
	"io"
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
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/empty"
	"github.com/google/go-containerregistry/pkg/v1/mutate"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/static"
	"github.com/spf13/afero"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/codec"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
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

// input builds an Input around a module source.
func input(t *testing.T, module map[string]any) *structpb.Struct {
	t.Helper()
	return inputWith(t, map[string]any{"module": module})
}

// inputWith builds an Input from its top-level fields (module, policy,
// limits, sandbox, config).
func inputWith(t *testing.T, fields map[string]any) *structpb.Struct {
	t.Helper()
	body := map[string]any{
		"apiVersion": "wasm.fn.crossplane.io/v1beta1",
		"kind":       "Input",
	}
	for k, v := range fields {
		body[k] = v
	}
	in, err := structpb.NewStruct(body)
	if err != nil {
		t.Fatal(err)
	}
	return in
}

func pathModule(name string) map[string]any {
	return map[string]any{"type": "Path", "path": name}
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

// publicRegistry serves an in-memory registry open to anonymous pulls.
func publicRegistry(t *testing.T) (host string) {
	t.Helper()
	srv := httptest.NewServer(registry.New())
	t.Cleanup(srv.Close)
	return strings.TrimPrefix(srv.URL, "http://")
}

// push publishes wasm as <ref> and returns its digest reference.
func push(t *testing.T, ref string, wasm []byte) string {
	t.Helper()
	return pushLayers(t, ref, static.NewLayer(wasm, "application/wasm"))
}

// pushWithManifest publishes wasm with a module-manifest layer beside it.
func pushWithManifest(t *testing.T, ref string, wasm []byte, manifestJSON string) string {
	t.Helper()
	return pushLayers(t, ref, static.NewLayer(wasm, "application/wasm"), static.NewLayer([]byte(manifestJSON), manifest.LayerMediaType))
}

func pushLayers(t *testing.T, ref string, layers ...v1.Layer) string {
	t.Helper()
	img, err := mutate.AppendLayers(empty.Image, layers...)
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
	d, err := img.Digest()
	if err != nil {
		t.Fatal(err)
	}
	return parsed.Context().Digest(d.String()).String()
}

func TestRunFunction(t *testing.T) {
	okModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	emptyModule := testwasm.Fixed(t, &fnv1.RunFunctionResponse{}, testwasm.Options{Body: "(i64.const 0)"})
	trapModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{Body: "unreachable"})
	loopModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{Body: "(loop $l (br $l)) (i64.const 0)"})
	growModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{Body: "(if (i32.eq (memory.grow (i32.const 64)) (i32.const -1)) (then unreachable)) (i64.const 0)"})

	// Sandbox guests: each returns what it read from its private /tmp or its
	// environment as a result message; one tries the default sandbox's
	// (absent) pre-open.
	readFileModule := testwasm.Fixed(t, guestResponse(), testwasm.ReadFile("hello.txt"))
	privateTmpModule := testwasm.Fixed(t, guestResponse(), testwasm.WriteRead("scratch.txt", "written by the guest"))
	environModule := testwasm.Fixed(t, guestResponse(), testwasm.Environ())

	moduleDir := t.TempDir()
	for name, wasm := range map[string][]byte{"fn.wasm": okModule, "empty.wasm": emptyModule, "trap.wasm": trapModule, "loop.wasm": loopModule, "grow.wasm": growModule, "notwasm.wasm": []byte("hello"),
		"readfile.wasm": readFileModule, "privatetmp.wasm": privateTmpModule, "environ.wasm": environModule} {
		if err := os.WriteFile(filepath.Join(moduleDir, name), wasm, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	ceiling, err := sandbox.NewCeiling(sandbox.Options{EnablePrivateTmp: true, EnableEnv: true})
	if err != nil {
		t.Fatal(err)
	}
	// sandboxed is what the sandbox guests return through the host: no meta
	// of their own (the host fills it), one normal result carrying the bytes.
	sandboxed := func(msg string) *fnv1.RunFunctionResponse {
		return &fnv1.RunFunctionResponse{
			Meta:    &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)},
			Results: []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: msg}},
		}
	}
	registryHost := privateRegistry(t)
	ociRef := push(t, registryHost+"/fn:v1", okModule)
	publicHost := publicRegistry(t)
	publicRef := push(t, publicHost+"/fn:v1", okModule)

	// An XR whose status names modules, for module.from: a public one, and
	// a private one that spends the step's credentials.
	xr := resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"},"spec":{"module":"fn.wasm"},"status":{"module":{"ref":"` + publicRef + `"},"private":{"ref":"` + ociRef + `","credentials":"registry"}}}`)
	privateRepo := ociRef[:strings.Index(ociRef, "/")+1]
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
		"NoType": {
			reason: "A module without a type is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"path": "fn.wasm"}),
			}},
			want: want{rsp: fatal("cannot resolve module: module.type is required: OCI, HTTP or Path")},
		},
		"NoSource": {
			reason: "A type without its object or from is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI"}),
			}},
			want: want{rsp: fatal("cannot resolve module: module.type OCI needs exactly one of module.oci and module.from")},
		},
		"MismatchedObject": {
			reason: "An object of another type than module.type is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI", "path": "fn.wasm"}),
			}},
			want: want{rsp: fatal("cannot resolve module: module.path is set but module.type is OCI")},
		},
		"PathModule": {
			reason: "A module served from the module directory runs and its response is returned verbatim.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, pathModule("fn.wasm")),
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIModule": {
			reason: "A public OCI module is pulled anonymously.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": publicRef}}),
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIModuleWithCredentials": {
			reason: "An OCI module is pulled with the pipeline step's credentials.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": ociRef, "credentials": "registry"}}),
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"OCITagRefused": {
			reason: "A tag reference is a fatal result: only digest-pinned references are accepted.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": registryHost + "/fn:v1"}}),
			}},
			want: want{rsp: fatal(`cannot resolve module: module.oci.ref "` + registryHost + `/fn:v1" must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported`)},
		},
		"OCIModuleMissingCredentials": {
			reason: "Naming a credential the step does not carry is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": ociRef, "credentials": "registry"}}),
			}},
			want: want{rsp: fatal(`cannot get credentials "registry" for module.oci: registry: credential not found`)},
		},
		"StaticIgnoresPolicy": {
			reason: "The policy fences XR-chosen modules only; a module the Composition names is pulled with its credentials whatever the policy says.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "oci": map[string]any{"ref": ociRef, "credentials": "registry"}}, "policy": map[string]any{"repositoryAllowList": []any{"ghcr.io/example-org/"}}}),
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIFromStatus": {
			reason: "module.from reads the OCI source from the observed XR and pulls it anonymously, within the repositories the Composition fenced it to.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.module"}, "policy": map[string]any{"repositoryAllowList": []any{publicHost + "/"}}}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIFromUnfenced": {
			reason: "A module the XR chooses requires policy.repositoryAllowList: without it the XR author could point the runtime at any host and read what its answer says.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"type": "OCI", "from": "status.module"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal("cannot resolve module: module.from: status.module of the composite resource names a OCI source, but policy.repositoryAllowList is not set: a module the composite resource chooses must be fenced to repositories the Composition names, or its author could point the runtime at any host")},
		},
		"OCIFromCredentialsRefused": {
			reason: "Fenced but without a credentials list, a module the XR chooses cannot spend the step's credentials: the XR author would pick the registry host they are sent to.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.private"}, "policy": map[string]any{"repositoryAllowList": []any{registryHost + "/"}}}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.private of the composite resource names credentials "registry", but a module chosen by the composite resource cannot use the step's credentials (the registry host would be its author's) unless policy.credentialsAllowList allows them for a repository in policy.repositoryAllowList; otherwise pull it with the runtime's Docker config or anonymously`)},
		},
		"OCIFromCredentialsAllowed": {
			reason: "A policy naming the credential and the private registry lets the XR-chosen module spend it, and the guest does not see the credential.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.private"}, "policy": map[string]any{"repositoryAllowList": []any{privateRepo}, "credentialsAllowList": []any{"registry"}}}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIFromCredentialsOutsideList": {
			reason: "A credential the policy does not list is refused.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.private"}, "policy": map[string]any{"repositoryAllowList": []any{privateRepo}, "credentialsAllowList": []any{"other"}}}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.private of the composite resource names credentials "registry", which policy.credentialsAllowList does not allow (allowed: other)`)},
		},
		"OCIFromRepositoryRefused": {
			reason: "An XR-chosen ref outside the repository allow list is a fatal result naming the policy and the ref.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.module"}, "policy": map[string]any{"repositoryAllowList": []any{"ghcr.io/example-org/"}}}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.module of the composite resource names ref "` + publicHost + `/fn", which policy.repositoryAllowList does not admit (allowed prefixes: ghcr.io/example-org/)`)},
		},
		"CredentialsListWithoutRepositories": {
			reason: "A credentials allow list without a repository allow list is refused: a credential must never be spendable on an arbitrary host.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "policy": map[string]any{"credentialsAllowList": []any{"registry"}}}),
			}},
			want: want{rsp: fatal("cannot resolve module: policy.credentialsAllowList requires policy.repositoryAllowList: a step credential must only be spent on repositories the Composition names")},
		},
		"PathFromSpec": {
			reason: "module.from with type Path reads the module path from the observed XR.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"type": "Path", "from": "spec.module"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: guestResponse()},
		},
		"FromMissingField": {
			reason: "A from field the XR lacks is a fatal result naming it.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"type": "OCI", "from": "status.other"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal("cannot resolve module: module.from: cannot read status.other from the composite resource: status.other: no such field")},
		},
		"FromWrongShape": {
			reason: "An XR field that does not hold what module.type expects is a fatal result naming the shape.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"type": "HTTP", "from": "status.module"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.module of the composite resource is not a {url, digest} object: json: unknown field "ref"`)},
		},
		"FromWithoutComposite": {
			reason: "A from source without an observed XR is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "Path", "from": "spec.module"}),
			}},
			want: want{rsp: fatal("cannot resolve module: module.from spec.module: no observed composite resource to read it from")},
		},
		"LimitsWithinCeilings": {
			reason: "Limits at most the runtime's ceilings apply and the module runs.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "limits": map[string]any{"timeout": "5s", "memory": "128Mi"}}),
			}},
			want: want{rsp: guestResponse()},
		},
		"LimitsTimeoutHit": {
			reason: "A run is interrupted at the Input's timeout, below the runtime's; the fatal result names the budget that applied.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("loop.wasm"), "limits": map[string]any{"timeout": "50ms"}}),
			}},
			want: want{rsp: fatal("module module file loop.wasm failed: wasmfn_run failed: module exceeded its execution deadline (50ms)")},
		},
		"LimitsMemoryHit": {
			reason: "The Input's memory limit applies to the run: growth past it fails inside the guest.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("grow.wasm"), "limits": map[string]any{"memory": "192Ki"}}),
			}},
			want: want{rsp: fatal("module module file grow.wasm failed: wasmfn_run failed: trap: unreachable code reached (a Go guest prints the panic to stderr)")},
		},
		"LimitsMemoryNotHit": {
			reason: "The same growth fits a larger Input limit; the guest then returns its (empty) response.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("grow.wasm"), "limits": map[string]any{"memory": "8Mi"}}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)}}},
		},
		"LimitsTimeoutAboveCeiling": {
			reason: "A timeout above --module-timeout is a fatal result naming both values.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "limits": map[string]any{"timeout": "1m"}}),
			}},
			want: want{rsp: fatal("limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s")},
		},
		"LimitsMemoryAboveCeiling": {
			reason: "A memory limit above --module-memory-limit is a fatal result naming both values.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "limits": map[string]any{"memory": "1Gi"}}),
			}},
			want: want{rsp: fatal("limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi")},
		},
		"LimitsTimeoutNotPositive": {
			reason: "A zero timeout is a mistake, not no limit.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "limits": map[string]any{"timeout": "0s"}}),
			}},
			want: want{rsp: fatal("limits.timeout 0s must be positive")},
		},
		"LimitsMemoryNotPositive": {
			reason: "A zero memory limit is a mistake, not no limit.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "limits": map[string]any{"memory": "0"}}),
			}},
			want: want{rsp: fatal("limits.memory 0 must be positive")},
		},
		"SandboxEmpty": {
			reason: "A sandbox that asks for nothing is the default sandbox.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "sandbox": map[string]any{"filesystem": map[string]any{}}}),
			}},
			want: want{rsp: guestResponse()},
		},
		"SandboxPrivateTmp": {
			reason: "The private /tmp is writable for the run: the guest writes a file and reads it back.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("privatetmp.wasm"), "sandbox": map[string]any{"filesystem": map[string]any{"privateTmp": true}}}),
			}},
			want: want{rsp: sandboxed("written by the guest")},
		},
		"SandboxEnv": {
			reason: "The environment the Composition grants is what the guest sees.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("environ.wasm"), "sandbox": map[string]any{"env": []any{map[string]any{"name": "GREETING", "value": "hello"}}}}),
			}},
			want: want{rsp: sandboxed("GREETING=hello\x00")},
		},
		"SandboxEnvValueFrom": {
			reason: "A valueFrom reads the value from a step credential.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": pathModule("environ.wasm"), "sandbox": map[string]any{"env": []any{map[string]any{"name": "PASSWORD", "valueFrom": map[string]any{"credential": map[string]any{"name": "registry", "key": "password"}}}}}}),
				Credentials: credentials,
			}},
			want: want{rsp: sandboxed("PASSWORD=s3cret\x00")},
		},
		"SandboxEnvFrom": {
			reason: "EnvFrom imports every key of a credential as environment variables.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": pathModule("environ.wasm"), "sandbox": map[string]any{"envFrom": []any{map[string]any{"credential": map[string]any{"name": "registry"}, "prefix": "REG_"}}}}),
				Credentials: credentials,
			}},
			// The environ guest returns sorted key=value pairs.
			want: want{rsp: sandboxed("REG_password=s3cret\x00REG_username=robot\x00")},
		},
		"SandboxEnvPullCredentialRefused": {
			reason: "The pull credential is withheld from env sources: a Composition cannot leak the module's registry secret into its environment.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "oci": map[string]any{"ref": ociRef, "credentials": "registry"}}, "sandbox": map[string]any{"env": []any{map[string]any{"name": "X", "valueFrom": map[string]any{"credential": map[string]any{"name": "registry", "key": "password"}}}}}}),
				Credentials: credentials,
			}},
			want: want{rsp: fatal(`sandbox.env[0].valueFrom.credential: credential "registry" is the pull credential and cannot be used as a source`)},
		},
		"SandboxDefaultIsClosed": {
			reason: "Without a grant a guest has no pre-opened directory: the same guest exits with EBADF (8) at path_open.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, pathModule("readfile.wasm")),
			}},
			want: want{rsp: fatal("module module file readfile.wasm failed: wasmfn_run failed: module exited with status 8")},
		},
		"SandboxInvalid": {
			reason: "A malformed sandbox is a fatal result naming the field.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "sandbox": map[string]any{"egress": map[string]any{"http": []any{map[string]any{"host": "api.example.com", "hostPattern": "*.example.com", "methods": []any{"GET"}}}}}}),
			}},
			want: want{rsp: fatal("sandbox.egress.http[0] must set exactly one of host and hostPattern")},
		},
		"MetaFilledForEmptyResponse": {
			reason: "A guest that returns nothing still yields a well-formed response.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, pathModule("empty.wasm")),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)}}},
		},
		"GuestTrap": {
			reason: "A trapping guest is a fatal result naming the module.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, pathModule("trap.wasm")),
			}},
			want: want{rsp: fatal("module module file trap.wasm failed: wasmfn_run failed: trap: unreachable code reached (a Go guest prints the panic to stderr)")},
		},
		"NotAModule": {
			reason: "Bytes that do not compile are a fatal result at load time.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, pathModule("notwasm.wasm")),
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
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, sandbox: ceiling}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			rsp, err := f.RunFunction(context.Background(), tc.args.req)

			// function-sdk-go decodes the Input with json/v2, whose semantic
			// errors say "cannot" or "unable to" — chosen once per process,
			// deliberately, so nobody depends on the wording. We only care
			// that the message is the decoder's.
			for _, r := range rsp.GetResults() {
				r.Message = strings.ReplaceAll(r.GetMessage(), "unable to ", "cannot ")
			}
			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.err, err, cmpopts.EquateErrors()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want err, +got err:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestRunFunctionSandboxCeiling pins that a runtime started without the
// --enable-sandbox-* flags refuses every filesystem and environment grant
// with a fatal result naming the grant and the flag, before any module is
// resolved — the default sandbox is the ceiling.
func TestRunFunctionSandboxCeiling(t *testing.T) {
	cases := map[string]struct {
		reason  string
		sandbox map[string]any
		want    string
	}{
		"PrivateTmp": {reason: "The private /tmp needs --enable-sandbox-private-tmp.", sandbox: map[string]any{"filesystem": map[string]any{"privateTmp": true}}, want: "sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp"},
		"Env":        {reason: "Environment variables need --enable-sandbox-env.", sandbox: map[string]any{"env": []any{map[string]any{"name": "GREETING", "value": "hello"}}}, want: "sandbox.env is refused: the runtime was started without --enable-sandbox-env"},
	}

	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{Dir: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	// No ceiling at all — what a Function gets when nothing is enabled.
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			rsp, err := f.RunFunction(context.Background(), &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("missing.wasm"), "sandbox": tc.sandbox}),
			})
			if err != nil {
				t.Fatalf("\n%s\nRunFunction(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(fatal(tc.want), rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestRunFunctionFuel pins that --enable-fuel counts instructions and
// limits.instructions caps the run. A module that runs past the budget is a
// fatal result; without the flag, limits.instructions is refused.
func TestRunFunctionFuel(t *testing.T) {
	loopModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{Body: "(loop $l (br $l)) (i64.const 0)"})
	okModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	moduleDir := t.TempDir()
	for name, wasm := range map[string][]byte{"loop.wasm": loopModule, "fn.wasm": okModule} {
		if err := os.WriteFile(filepath.Join(moduleDir, name), wasm, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	cases := map[string]struct {
		reason       string
		fuel         bool
		instructions any
		moduleName   string
		want         string
	}{
		"InstructionsWithoutFuel": {
			reason:       "limits.instructions without --enable-fuel is refused.",
			fuel:         false,
			instructions: 1000000,
			moduleName:   "fn.wasm",
			want:         "limits.instructions is refused: the runtime was started without --enable-fuel",
		},
		"FuelExhausted": {
			reason:       "A looping guest past its instruction budget is a fatal result.",
			fuel:         true,
			instructions: 100_000,
			moduleName:   "loop.wasm",
			want:         "module module file loop.wasm failed: wasmfn_run failed: module exceeded its instruction budget (100000 instructions)",
		},
		"FuelSufficient": {
			reason:     "A guest within its instruction budget succeeds.",
			fuel:       true,
			moduleName: "fn.wasm",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			eng, err := engine.New(engine.Config{Fuel: tc.fuel, InstructionLimit: 100_000_000})
			if err != nil {
				t.Fatal(err)
			}
			defer eng.Close()
			resolver, err := module.NewResolver(module.Options{Dir: moduleDir})
			if err != nil {
				t.Fatal(err)
			}
			f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}

			inputFields := map[string]any{"module": pathModule(tc.moduleName)}
			if tc.instructions != nil {
				inputFields["limits"] = map[string]any{"instructions": tc.instructions}
			}
			rsp, err := f.RunFunction(context.Background(), &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, inputFields),
			})
			if err != nil {
				t.Fatalf("\n%s\nRunFunction(): unexpected error: %v", tc.reason, err)
			}
			if tc.want == "" {
				if diff := cmp.Diff(guestResponse(), rsp, protocmp.Transform()); diff != "" {
					t.Errorf("\n%s\nRunFunction(): -want, +got:\n%s", tc.reason, diff)
				}
				return
			}
			if diff := cmp.Diff(fatal(tc.want), rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestSandboxFlags pins the shape of the sandbox flags: each capability has
// its --enable-sandbox-<feature> switch, off by default, readable from the
// environment; there is no flag that mounts a host directory. serve is the
// default command, so the flags need no subcommand and a
// DeploymentRuntimeConfig's args keep working; validate takes the same
// ceiling flags.
func TestSandboxFlags(t *testing.T) {
	t.Setenv("ENABLE_SANDBOX_ENV", "true")
	var c CLI
	p := parser(&c, io.Discard)
	ctx, err := p.Parse([]string{"--enable-sandbox-private-tmp", "--insecure"})
	if err != nil {
		t.Fatal(err)
	}
	if ctx.Command() != "serve" {
		t.Errorf("flags without a subcommand select %q, want serve", ctx.Command())
	}
	if _, err := p.Parse([]string{"--enable-sandbox-mounts"}); err == nil {
		t.Fatal("--enable-sandbox-mounts must not exist: host directories are not mountable into modules")
	}
	ceilings := CeilingFlags{MaxModuleSize: 128, ModuleTimeout: 30 * time.Second, ModuleMemoryLimit: 512, EnableSandboxPrivateTmp: true, EnableSandboxEnv: true}
	want := ServeCmd{
		CeilingFlags: ceilings,
		Network:      "tcp", Address: ":9443", Insecure: true, MaxRecvMessageSize: 4, EnableMemoryCache: true, MaxConcurrentCompiles: 1, HealthAddress: ":8081",
	}
	if diff := cmp.Diff(want, c.Serve); diff != "" {
		t.Errorf("serve flags: -want, +got:\n%s", diff)
	}

	c = CLI{}
	ctx, err = parser(&c, io.Discard).Parse([]string{"validate", "composition.yaml", "--enable-sandbox-private-tmp", "--output", "json"})
	if err != nil {
		t.Fatal(err)
	}
	if ctx.Command() != "validate <file>" {
		t.Errorf("validate selects %q", ctx.Command())
	}
	if diff := cmp.Diff(ValidateCmd{CeilingFlags: ceilings, Files: []string{"composition.yaml"}, Output: "json"}, c.Validate, cmpopts.IgnoreUnexported(ValidateCmd{})); diff != "" {
		t.Errorf("validate flags: -want, +got:\n%s", diff)
	}
}

// TestRunFunctionVerifiesBeforeServing pins that a compiled artifact left on
// disk by a runtime without a key is not served by one with a key: signature
// verification is a precondition of running, not of fetching.
func TestRunFunctionVerifiesBeforeServing(t *testing.T) {
	okModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	ref := push(t, publicRegistry(t)+"/fn:v1", okModule)
	req := &fnv1.RunFunctionRequest{
		Meta:  &fnv1.RequestMeta{Tag: "hello"},
		Input: input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": ref}}),
	}

	eng, err := engine.New(engine.Config{Timeout: 5 * time.Second})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	disk := cache.New(afero.NewMemMapFs(), false)

	unkeyed, err := module.NewResolver(module.Options{})
	if err != nil {
		t.Fatal(err)
	}
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{Disk: disk}), resolver: unkeyed}
	rsp, _ := f.RunFunction(context.Background(), req)
	if diff := cmp.Diff(guestResponse(), rsp, protocmp.Transform()); diff != "" {
		t.Fatalf("unkeyed runtime: -want, +got:\n%s", diff)
	}
	if disk.Len() != 1 {
		t.Fatalf("want the compiled artifact on disk, have %d entries", disk.Len())
	}

	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	der, err := x509.MarshalPKIXPublicKey(&key.PublicKey)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := module.NewVerifier(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: der}))
	if err != nil {
		t.Fatal(err)
	}
	keyed, err := module.NewResolver(module.Options{Verifier: verifier})
	if err != nil {
		t.Fatal(err)
	}
	f = &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{Disk: disk}), resolver: keyed}
	rsp, _ = f.RunFunction(context.Background(), req)
	want := fatal("cannot verify module oci " + ref + ": " + ref + " carries no cosign signature (" + strings.SplitN(ref, "@", 2)[0] + ":sha256-" + strings.TrimPrefix(strings.SplitN(ref, "@", 2)[1], "sha256:") + ".sig not found)")
	if diff := cmp.Diff(want, rsp, protocmp.Transform()); diff != "" {
		t.Errorf("keyed runtime must refuse the unsigned module even with its artifact on disk: -want, +got:\n%s", diff)
	}
}

// TestRunFunctionRecoversPanics pins that a host-side panic is a fatal
// result, not the end of the process: gRPC does not recover handler panics.
func TestRunFunctionRecoversPanics(t *testing.T) {
	eng, err := engine.New(engine.Config{Timeout: time.Second})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "fn.wasm"), testwasm.Fixed(t, guestResponse(), testwasm.Options{}), 0o600); err != nil {
		t.Fatal(err)
	}
	resolver, err := module.NewResolver(module.Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	// A nil cache makes the load path dereference nil — a stand-in for any
	// bug on the host side of a request.
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: nil, resolver: resolver}
	rsp, err := f.RunFunction(context.Background(), &fnv1.RunFunctionRequest{
		Meta:  &fnv1.RequestMeta{Tag: "hello"},
		Input: input(t, pathModule("fn.wasm")),
	})
	if err != nil {
		t.Fatalf("RunFunction() must not return a gRPC error for a panic: %v", err)
	}
	if len(rsp.GetResults()) != 1 || rsp.GetResults()[0].GetSeverity() != fnv1.Severity_SEVERITY_FATAL || !strings.HasPrefix(rsp.GetResults()[0].GetMessage(), "internal error while running the module: ") {
		t.Errorf("want one fatal result naming an internal error, got %v", rsp.GetResults())
	}
}

// TestRunFunctionRawPath pins the raw-bytes codec path: when the codec
// stashes the request's wire bytes and no pull credential needs stripping,
// the host forwards them to the guest without re-marshaling. When a
// credential does need stripping, the normal path runs instead and the
// guest never sees the pull credential.
func TestRunFunctionRawPath(t *testing.T) {
	okModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	moduleDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(moduleDir, "fn.wasm"), okModule, 0o600); err != nil {
		t.Fatal(err)
	}

	registryHost := privateRegistry(t)
	ociRef := push(t, registryHost+"/fn:v1", okModule)
	credentials := map[string]*fnv1.Credentials{
		"registry": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
			Data: map[string][]byte{"username": []byte("robot"), "password": []byte("s3cret")},
		}}},
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
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}

	t.Run("RawPathNoCredential", func(t *testing.T) {
		req := &fnv1.RunFunctionRequest{
			Meta:  &fnv1.RequestMeta{Tag: "hello"},
			Input: input(t, pathModule("fn.wasm")),
		}
		// Simulate what the gRPC codec does: stash the raw wire bytes.
		raw, err := proto.Marshal(req)
		if err != nil {
			t.Fatal(err)
		}
		codec.StashRequest(req, raw)

		rsp, err := f.RunFunction(context.Background(), req)
		if err != nil {
			t.Fatalf("RunFunction(): unexpected error: %v", err)
		}
		if diff := cmp.Diff(guestResponse(), rsp, protocmp.Transform()); diff != "" {
			t.Errorf("raw path response: -want, +got:\n%s", diff)
		}
	})

	t.Run("NormalPathWithCredential", func(t *testing.T) {
		req := &fnv1.RunFunctionRequest{
			Meta:        &fnv1.RequestMeta{Tag: "hello"},
			Input:       input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": ociRef, "credentials": "registry"}}),
			Credentials: credentials,
		}
		// Stash raw bytes - they include the credential. The raw path
		// must not be taken because the credential needs stripping.
		raw, err := proto.Marshal(req)
		if err != nil {
			t.Fatal(err)
		}
		codec.StashRequest(req, raw)

		rsp, err := f.RunFunction(context.Background(), req)
		if err != nil {
			t.Fatalf("RunFunction(): unexpected error: %v", err)
		}
		if diff := cmp.Diff(guestResponse(), rsp, protocmp.Transform()); diff != "" {
			t.Errorf("normal path response: -want, +got:\n%s", diff)
		}
	})

	t.Run("FallbackWithoutStash", func(t *testing.T) {
		req := &fnv1.RunFunctionRequest{
			Meta:  &fnv1.RequestMeta{Tag: "hello"},
			Input: input(t, pathModule("fn.wasm")),
		}
		// No stash - the normal path must work as before.
		rsp, err := f.RunFunction(context.Background(), req)
		if err != nil {
			t.Fatalf("RunFunction(): unexpected error: %v", err)
		}
		if diff := cmp.Diff(guestResponse(), rsp, protocmp.Transform()); diff != "" {
			t.Errorf("fallback response: -want, +got:\n%s", diff)
		}
	})
}
