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
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

const ttl = 60 * time.Second

// permissiveSandboxPolicy is an operator grant policy that enables every
// sandbox capability for any caller - a fully open operator layer, for tests
// that exercise a granted sandbox. The layer is the enabler, so a Function
// without one grants nothing.
func permissiveSandboxPolicy(t *testing.T) *authz.OperatorPolicy {
	t.Helper()
	p, err := authz.NewOperatorPolicy("test.cedar", []byte(`
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
permit (principal, action == Action::"spendCredential", resource);
`))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

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

// inputWith builds an Input from its top-level fields (module,
// compositionPolicy, limits, config).
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
	ceiling, err := sandbox.NewCeiling(sandbox.Options{})
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
	privateRepo := ociRef[:strings.Index(ociRef, "/")+1]

	// The sandbox guests carry their requests as manifests: the module asks,
	// the policy layers grant (path modules have no manifest, so they can
	// request nothing).
	tmpRef := pushWithManifest(t, publicHost+"/tmp:v1", privateTmpModule, `{"abi":1,"requires":{"filesystem":{"privateTmp":true}}}`)
	envRef := pushWithManifest(t, publicHost+"/env:v1", environModule, `{"abi":1,"requires":{"env":[{"name":"PASSWORD","fromCredential":{"name":"registry","key":"password"}}]}}`)
	envPullRef := pushWithManifest(t, registryHost+"/envpull:v1", environModule, `{"abi":1,"requires":{"env":[{"name":"PASSWORD","fromCredential":{"name":"registry","key":"password"}}]}}`)

	// The composition policies the from cases carry: a pullModule fence over
	// the public registry, and one that also permits spending the step's
	// registry credential on the private one.
	fencedPolicy := `permit (principal, action == Action::"pullModule", resource in Repository::"` + publicHost + `");`
	privateFence := `permit (principal, action == Action::"pullModule", resource in Repository::"` + privateRepo + `");`
	trustedPolicy := privateFence + `
permit (principal, action == Action::"spendCredential", resource == Credential::"registry")
when { context.repository in Repository::"` + privateRepo + `" };`
	otherCredPolicy := privateFence + `
permit (principal, action == Action::"spendCredential", resource == Credential::"other");`

	// An XR whose status names modules, for module.from: a public one, and
	// a private one that spends the step's credentials.
	xr := resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"},"spec":{"module":"fn.wasm"},"status":{"module":{"ref":"` + publicRef + `"},"private":{"ref":"` + ociRef + `","credentials":"registry"}}}`)
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
		"StaticIgnoresFence": {
			reason: "The compositionPolicy's pullModule fence covers XR-chosen modules only; a module the Composition names is pulled with its credentials whatever the policy permits.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "oci": map[string]any{"ref": ociRef, "credentials": "registry"}}, "compositionPolicy": `permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example-org");`}),
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIFromStatus": {
			reason: "module.from reads the OCI source from the observed XR and pulls it anonymously, within the repositories the compositionPolicy permits (pullModule).",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.module"}, "compositionPolicy": fencedPolicy}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIFromUnfenced": {
			reason: "A module the XR chooses requires a compositionPolicy: without one the XR author could point the runtime at any host and read what its answer says.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    input(t, map[string]any{"type": "OCI", "from": "status.module"}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal("cannot resolve module: module.from: status.module of the composite resource names a OCI source, but the Input has no compositionPolicy: a module the composite resource chooses must be permitted by the compositionPolicy's pullModule rules, or its author could point the runtime at any host")},
		},
		"OCIFromCredentialsRefused": {
			reason: "Fenced for pulling but without a spendCredential permit, a module the XR chooses cannot spend the step's credentials: the XR author would pick the registry host they are sent to.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.private"}, "compositionPolicy": privateFence}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.private of the composite resource names credentials "registry", which the compositionPolicy does not permit (spendCredential) for "` + registryHost + `/fn": a module chosen by the composite resource cannot spend a step credential (the registry host would be its author's) unless the compositionPolicy permits it for that repository; otherwise pull it with the runtime's Docker config or anonymously`)},
		},
		"OCIFromCredentialsAllowed": {
			reason: "A compositionPolicy permitting the credential for the private repository lets the XR-chosen module spend it, and the guest does not see the credential.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.private"}, "compositionPolicy": trustedPolicy}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: guestResponse()},
		},
		"OCIFromCredentialsOutsideList": {
			reason: "A credential no spendCredential permit names is refused.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.private"}, "compositionPolicy": otherCredPolicy}),
				Observed:    &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
				Credentials: credentials,
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.private of the composite resource names credentials "registry", which the compositionPolicy does not permit (spendCredential) for "` + registryHost + `/fn": a module chosen by the composite resource cannot spend a step credential (the registry host would be its author's) unless the compositionPolicy permits it for that repository; otherwise pull it with the runtime's Docker config or anonymously`)},
		},
		"OCIFromRepositoryRefused": {
			reason: "An XR-chosen ref no pullModule permit admits is a fatal result naming the policy and the ref.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    inputWith(t, map[string]any{"module": map[string]any{"type": "OCI", "from": "status.module"}, "compositionPolicy": `permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example-org");`}),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}},
			want: want{rsp: fatal(`cannot resolve module: module.from: status.module of the composite resource names ref "` + publicHost + `/fn", which the compositionPolicy does not permit (pullModule)`)},
		},
		"MalformedCompositionPolicy": {
			reason: "Malformed Cedar in compositionPolicy is a fatal result at admission, before anything is resolved.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "compositionPolicy": "permit (principal"}),
			}},
			want: want{rsp: fatal(`compositionPolicy is invalid: cannot compile the compositionPolicy as Cedar: parser error: parse error at <input>:1:18 "": exact got  want ,`)},
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
		"RemovedFieldsRefused": {
			reason: "The removed policy and sandbox Input fields fail the runtime's strict decode: an unported Composition is refused, never silently stripped of its old grants.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "sandbox": map[string]any{"filesystem": map[string]any{"privateTmp": true}}, "policy": map[string]any{"repositoryAllowList": []any{"x/"}}}),
			}},
			want: want{rsp: fatal(`cannot get function input from *v1.RunFunctionRequest: cannot get function input *v1beta1.Input from *v1.RunFunctionRequest: cannot unmarshal JSON from *structpb.Struct into *v1beta1.Input: json: cannot unmarshal JSON string into Go v1beta1.Input: unknown object member name "policy"`)},
		},
		"ManifestPrivateTmp": {
			reason: "A module whose manifest requires a private /tmp gets one where the operator layer permits: the guest writes a file and reads it back.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": tmpRef}}),
			}},
			want: want{rsp: sandboxed("written by the guest")},
		},
		"ManifestEnvBinding": {
			reason: "A manifest env binding resolves from the step credential: the module owns its env contract, the layers grant it.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": envRef}}),
				Credentials: credentials,
			}},
			want: want{rsp: sandboxed("PASSWORD=s3cret\x00")},
		},
		"ManifestEnvBindingMissingCredential": {
			reason: "A binding whose credential the request does not carry is a fatal result telling the author where to declare it.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": envRef}}),
			}},
			want: want{rsp: fatal(`module oci ` + envRef + `: requires.env[0] (PASSWORD): the request carries no credential "registry"; declare it on the pipeline step`)},
		},
		"ManifestEnvPullCredentialRefused": {
			reason: "The pull credential is withheld from env bindings: a module cannot read its own registry secret into its environment.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:        &fnv1.RequestMeta{Tag: "hello"},
				Input:       input(t, map[string]any{"type": "OCI", "oci": map[string]any{"ref": envPullRef, "credentials": "registry"}}),
				Credentials: credentials,
			}},
			want: want{rsp: fatal(`module oci ` + envPullRef + `: requires.env[0] (PASSWORD): credential "registry" is the pull credential and cannot be used as a source`)},
		},
		"SandboxDefaultIsClosed": {
			reason: "Without a grant a guest has no pre-opened directory: the same guest exits with EBADF (8) at path_open.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: input(t, pathModule("readfile.wasm")),
			}},
			want: want{rsp: fatal("module module file readfile.wasm failed: wasmfn_run failed: module exited with status 8")},
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
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, sandbox: ceiling, policy: permissiveSandboxPolicy(t)}

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

// TestSandboxFlags pins the sandbox flag surface after enablement moved to
// Cedar: the per-capability --enable-sandbox-* switches are gone (the operator
// enables a capability through --sandbox-policy-file), and there is still no
// flag that mounts a host directory. serve is the default command, so the flags
// need no subcommand and a DeploymentRuntimeConfig's args keep working; validate
// takes the same ceiling flags.
func TestSandboxFlags(t *testing.T) {
	// The per-capability enable flags and the host-mount flag do not exist.
	for _, flag := range []string{"--enable-sandbox-private-tmp", "--enable-sandbox-env", "--enable-sandbox-egress", "--enable-sandbox-mounts"} {
		var c CLI
		if _, err := parser(&c, io.Discard).Parse([]string{flag, "--insecure"}); err == nil {
			t.Fatalf("%s must not exist: sandbox capabilities are enabled by the Cedar --sandbox-policy-file, and host directories are never mountable", flag)
		}
	}

	// --sandbox-policy-file is an existing file; a mounted ConfigMap satisfies it.
	policyFile := filepath.Join(t.TempDir(), "policy.cedar")
	if err := os.WriteFile(policyFile, []byte(`permit (principal, action == Action::"usePrivateTmp", resource);`), 0o600); err != nil {
		t.Fatal(err)
	}

	var c CLI
	ctx, err := parser(&c, io.Discard).Parse([]string{"--sandbox-policy-file", policyFile, "--insecure"})
	if err != nil {
		t.Fatal(err)
	}
	if ctx.Command() != "serve" {
		t.Errorf("flags without a subcommand select %q, want serve", ctx.Command())
	}
	ceilings := CeilingFlags{MaxModuleSize: 128, ModuleTimeout: 30 * time.Second, ModuleMemoryLimit: 512, SandboxPolicyFile: policyFile}
	want := ServeCmd{
		CeilingFlags: ceilings,
		Network:      "tcp", Address: ":9443", Insecure: true, MaxRecvMessageSize: 4, EnableMemoryCache: true, MaxConcurrentCompiles: 1, HealthAddress: ":8081",
	}
	if diff := cmp.Diff(want, c.Serve); diff != "" {
		t.Errorf("serve flags: -want, +got:\n%s", diff)
	}

	c = CLI{}
	ctx, err = parser(&c, io.Discard).Parse([]string{"validate", "composition.yaml", "--sandbox-policy-file", policyFile, "--output", "json"})
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
