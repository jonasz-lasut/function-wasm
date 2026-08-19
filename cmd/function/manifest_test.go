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

	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// TestRunFunctionManifest pins the three-layer decision between load and run:
// an artifact's manifest layer is the module's request, decided by the
// compositionPolicy and the operator's policy, and its config schema is held
// against the Input's config; the refusal names the module and the miss.
// Manifests are read once per digest into the on-disk store.
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
	otherHostPolicy, err := authz.NewOperatorPolicy("test.cedar", []byte(`permit (principal, action == Action::"grantEgress", resource in HostPattern::"other.net");`))
	if err != nil {
		t.Fatal(err)
	}
	store := cache.New(afero.NewMemMapFs(), false)
	f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: enabledEgress, policy: permissiveSandboxPolicy(t), manifests: store}
	// A runtime with no --sandbox-policy-file: the operator layer enables
	// nothing, so any manifest request is refused.
	closed := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: enabledEgress, manifests: store}
	// A policy that permits egress only to another host: the operator layer
	// is default-deny within too.
	narrow := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: enabledEgress, policy: otherHostPolicy, manifests: store}

	cases := map[string]struct {
		reason string
		fn     *Function
		fields map[string]any
		want   *fnv1.RunFunctionResponse
	}{
		"NoManifest": {
			reason: "A module without a manifest runs with the default sandbox, whatever the policies would permit.",
			fn:     f,
			fields: map[string]any{"module": oci("plain")},
			want:   guestResponse(),
		},
		"EgressGranted": {
			reason: "A required egress rule both layers permit is granted: nothing to write in the Input.",
			fn:     f,
			fields: map[string]any{"module": oci("egress")},
			want:   guestResponse(),
		},
		"EgressNoPolicy": {
			reason: "Without an operator policy the request is refused before the module runs, naming the module and the missing layer.",
			fn:     closed,
			fields: map[string]any{"module": oci("egress")},
			want:   fatal("module oci " + refs["egress"] + " requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)"),
		},
		"EgressOperatorDenied": {
			reason: "An operator policy that does not permit the required host refuses it (default-deny within the layer).",
			fn:     narrow,
			fields: map[string]any{"module": oci("egress")},
			want:   fatal("module oci " + refs["egress"] + ` requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`),
		},
		"EgressCompositionNarrowed": {
			reason: "A compositionPolicy that scopes grantEgress opts into narrowing: the module's ask outside it is refused naming the composition layer.",
			fn:     f,
			fields: map[string]any{"module": oci("egress"), "compositionPolicy": `permit (principal, action == Action::"grantEgress", resource in HostPattern::"other.net");`},
			want:   fatal("module oci " + refs["egress"] + ` requires egress GET to host "api.example.com" (requires.egress.http[0]), which the compositionPolicy does not permit`),
		},
		"EgressCompositionNotScoping": {
			reason: "A compositionPolicy that scopes another action does not narrow egress: scoped default-permit.",
			fn:     f,
			fields: map[string]any{"module": oci("egress"), "compositionPolicy": `permit (principal, action == Action::"usePrivateTmp", resource);`},
			want:   guestResponse(),
		},
		"TmpGranted": {
			reason: "A required private /tmp the operator layer permits is granted.",
			fn:     f,
			fields: map[string]any{"module": oci("tmp")},
			want:   guestResponse(),
		},
		"TmpNoPolicy": {
			reason: "The unmet request is named with the missing layer.",
			fn:     closed,
			fields: map[string]any{"module": oci("tmp")},
			want:   fatal("module oci " + refs["tmp"] + " requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities"),
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
	// at hand, the registry gone — reads the manifest from the store: the
	// request layer still refuses without an operator policy.
	srv.Close()
	warm := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: f.modules, resolver: resolver, egress: enabledEgress, manifests: store}
	rsp, err := warm.RunFunction(context.Background(), &fnv1.RunFunctionRequest{Meta: &fnv1.RequestMeta{Tag: "hello"}, Input: inputWith(t, map[string]any{"module": oci("egress")})})
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(fatal("module oci "+refs["egress"]+" requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)"), rsp, protocmp.Transform()); diff != "" {
		t.Errorf("a stored manifest must be read without the registry: -want, +got:\n%s", diff)
	}
}
