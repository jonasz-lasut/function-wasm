package main

import (
	"context"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/go-containerregistry/pkg/registry"

	"github.com/google/go-cmp/cmp"
	"github.com/spf13/afero"
	"google.golang.org/protobuf/testing/protocmp"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// TestRunFunctionManifest pins the manifest check between load and run: an
// artifact's manifest layer is held against what the Composition was granted
// (and the operator admitted), narrowing only, and its config schema against
// the Input's config; the refusal names the module and the miss. Manifests
// are read once per digest into the on-disk store.
func TestRunFunctionManifest(t *testing.T) {
	wasm := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	srv := httptest.NewServer(registry.New())
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://")
	refs := map[string]string{}
	for name, m := range map[string]string{
		"egress":  `{"abi":1,"name":"greeter","version":"0.1.0","requires":{"egress":{"http":[{"host":"api.example.com","methods":["GET"],"pathPrefix":"/v1/"}]}}}`,
		"tmp":     `{"abi":1,"requires":{"filesystem":{"privateTmp":true}}}`,
		"schema":  `{"abi":1,"config":{"schema":{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"],"additionalProperties":false}}}`,
		"badabi":  `{"abi":2}`,
		"garbage": `not json`,
		"runtime": `{"abi":1,"minRuntime":"v99.0.0"}`,
	} {
		refs[name] = pushWithManifest(t, host+"/"+name+":v1", wasm, m)
	}
	refs["plain"] = push(t, host+"/plain:v1", wasm)
	oci := func(name string) map[string]any {
		return map[string]any{"type": "OCI", "oci": map[string]any{"ref": refs[name]}}
	}

	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{})
	if err != nil {
		t.Fatal(err)
	}
	enabledEgress, err := egress.New()
	if err != nil {
		t.Fatal(err)
	}
	ceiling, err := sandbox.NewCeiling(sandbox.Options{})
	if err != nil {
		t.Fatal(err)
	}
	store := cache.New(afero.NewMemMapFs(), false)
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: enabledEgress, sandbox: ceiling, policy: permissiveSandboxPolicy(t), manifests: store}
	closed := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}

	egressGrant := map[string]any{"egress": map[string]any{"http": []any{map[string]any{"host": "api.example.com", "methods": []any{"GET"}, "pathPrefix": "/v1/"}}}}
	cases := map[string]struct {
		reason string
		fn     *Function
		fields map[string]any
		want   *fnv1.RunFunctionResponse
	}{
		"NoManifest": {
			reason: "A module without a manifest runs as it always did.",
			fn:     f,
			fields: map[string]any{"module": oci("plain")},
			want:   guestResponse(),
		},
		"EgressGranted": {
			reason: "A required egress rule the Composition grants (and the operator admits) satisfies the manifest.",
			fn:     f,
			fields: map[string]any{"module": oci("egress"), "sandbox": egressGrant},
			want:   guestResponse(),
		},
		"EgressWiderGrant": {
			reason: "A wider grant covers a narrower requirement: more methods, a shorter prefix.",
			fn:     f,
			fields: map[string]any{"module": oci("egress"), "sandbox": map[string]any{"egress": map[string]any{"http": []any{map[string]any{"host": "api.example.com", "methods": []any{"GET", "POST"}}}}}},
			want:   guestResponse(),
		},
		"EgressNotGranted": {
			reason: "Without the grant the run is refused before it starts, naming the module and the rule.",
			fn:     f,
			fields: map[string]any{"module": oci("egress")},
			want:   fatal("module oci " + refs["egress"] + " requires sandbox.egress.http host api.example.com methods [GET] pathPrefix /v1/, which the Composition does not grant"),
		},
		"EgressNarrowerGrant": {
			reason: "A grant that does not cover the requirement (a longer prefix) is a miss.",
			fn:     f,
			fields: map[string]any{"module": oci("egress"), "sandbox": map[string]any{"egress": map[string]any{"http": []any{map[string]any{"host": "api.example.com", "methods": []any{"GET"}, "pathPrefix": "/v1/items/"}}}}},
			want:   fatal("module oci " + refs["egress"] + " requires sandbox.egress.http host api.example.com methods [GET] pathPrefix /v1/, which the Composition does not grant"),
		},
		"PolicyFirst": {
			reason: "Admission refuses before the manifest is consulted: a grant no policy enables is refused, so the manifest never sees what admission would refuse.",
			fn:     closed,
			fields: map[string]any{"module": oci("egress"), "sandbox": egressGrant},
			want:   fatal("sandbox.egress is refused: the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)"),
		},
		"TmpGranted": {
			reason: "A private /tmp, granted, satisfies the manifest.",
			fn:     f,
			fields: map[string]any{"module": oci("tmp"), "sandbox": map[string]any{"filesystem": map[string]any{"privateTmp": true}}},
			want:   guestResponse(),
		},
		"TmpNotGranted": {
			reason: "The unmet requirement is named.",
			fn:     f,
			fields: map[string]any{"module": oci("tmp")},
			want:   fatal("module oci " + refs["tmp"] + " requires sandbox.filesystem.privateTmp, which the Composition does not grant"),
		},
		"ConfigMatches": {
			reason: "A config within the module's schema runs.",
			fn:     f,
			fields: map[string]any{"module": oci("schema"), "config": map[string]any{"greeting": "hi"}},
			want:   guestResponse(),
		},
		"ConfigWrongType": {
			reason: "A config outside the schema is refused with the field and the reason.",
			fn:     f,
			fields: map[string]any{"module": oci("schema"), "config": map[string]any{"greeting": 7}},
			want:   fatal("module oci " + refs["schema"] + " config does not match the module's schema: /greeting: got number, want string"),
		},
		"ConfigMissing": {
			reason: "A required config key that is absent — no config at all — is a miss too.",
			fn:     f,
			fields: map[string]any{"module": oci("schema")},
			want:   fatal("module oci " + refs["schema"] + " config does not match the module's schema: /: missing property 'greeting'"),
		},
		"BadABI": {
			reason: "A manifest for another ABI is an invalid manifest.",
			fn:     f,
			fields: map[string]any{"module": oci("badabi")},
			want:   fatal("module oci " + refs["badabi"] + " has an invalid manifest: abi must be 1 (this runtime implements ABI v1), got 2"),
		},
		"Garbage": {
			reason: "A layer that is not a manifest is refused, not ignored.",
			fn:     f,
			fields: map[string]any{"module": oci("garbage")},
			want:   fatal("module oci " + refs["garbage"] + " has an invalid manifest: cannot parse manifest: invalid character 'o' in literal null (expecting 'u')"),
		},
		"MinRuntime": {
			reason: "A module needing a newer runtime is refused with both versions on a released runtime; a development build (this test binary) passes.",
			fn:     f,
			fields: map[string]any{"module": oci("runtime")},
			want:   guestResponse(),
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			rsp, err := tc.fn.RunFunction(context.Background(), &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, tc.fields),
			})
			if err != nil {
				t.Fatalf("\n%s\nRunFunction(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}

	// Every digest the Function saw has an entry in the store — the manifest
	// bytes, or an empty entry for a module without one — so a new process on
	// the same volume asks the registry nothing.
	for name, ref := range refs {
		digest := ref[len(ref)-71:]
		raw, ok := store.Get(digest)
		if !ok {
			t.Errorf("no store entry for %s", name)
			continue
		}
		if (name == "plain") != (len(raw) == 0) {
			t.Errorf("store entry for %s: %q", name, raw)
		}
	}
	// A new process on the same volume — the compiled modules and the store
	// at hand, the registry gone — reads the manifest from the store.
	srv.Close()
	warm := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: f.modules, resolver: resolver, egress: enabledEgress, sandbox: ceiling, manifests: store}
	rsp, err := warm.RunFunction(context.Background(), &fnv1.RunFunctionRequest{Meta: &fnv1.RequestMeta{Tag: "hello"}, Input: inputWith(t, map[string]any{"module": oci("egress")})})
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(fatal("module oci "+refs["egress"]+" requires sandbox.egress.http host api.example.com methods [GET] pathPrefix /v1/, which the Composition does not grant"), rsp, protocmp.Transform()); diff != "" {
		t.Errorf("a stored manifest must be read without the registry: -want, +got:\n%s", diff)
	}
}
