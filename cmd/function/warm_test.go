package main

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/alecthomas/kong"
	"github.com/google/go-cmp/cmp"
	"github.com/google/go-containerregistry/pkg/authn"
	"google.golang.org/protobuf/testing/protocmp"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// TestWarm pins warm-up: every entry goes through the request's own load
// path, a loaded module is a memory hit for its first request, and an entry
// that cannot be loaded is logged and changes nothing else.
func TestWarm(t *testing.T) {
	okModule := testwasm.Fixed(t, guestResponse(), testwasm.Options{})
	moduleDir := t.TempDir()
	for name, wasm := range map[string][]byte{"fn.wasm": okModule, "notwasm.wasm": []byte("hello")} {
		if err := os.WriteFile(filepath.Join(moduleDir, name), wasm, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	registryHost := publicRegistry(t)
	ociRef := push(t, registryHost+"/fn:v1", okModule)
	missingRef := registryHost + "/fn@sha256:" + strings.Repeat("0", 64)

	type args struct {
		entries  []string
		parallel int
	}
	type want struct {
		// loaded is how many modules the memory tier holds after warm-up.
		loaded int
		// failed are the entries reported as not warmed, with the reason.
		failed map[string]string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"Nothing": {
			reason: "No entries, nothing loaded, nothing logged.",
			want:   want{failed: map[string]string{}},
		},
		"PathModule": {
			reason: "A path: entry names a file under --module-dir and is compiled into the memory tier.",
			args:   args{entries: []string{"path:fn.wasm"}},
			want:   want{loaded: 1, failed: map[string]string{}},
		},
		"OCIModule": {
			reason: "A digest-pinned OCI reference is pulled and compiled into the memory tier.",
			args:   args{entries: []string{ociRef}},
			want:   want{loaded: 1, failed: map[string]string{}},
		},
		"SeveralInParallel": {
			reason: "Entries load side by side up to the compile slots; a file and an OCI artifact are two modules even with the same bytes (the file digest and the manifest digest key them).",
			args:   args{entries: []string{"path:fn.wasm", ociRef}, parallel: 2},
			want:   want{loaded: 2, failed: map[string]string{}},
		},
		"UnpinnedRef": {
			reason: "A tag reference is refused like it would be in an Input, and logged with the entry.",
			args:   args{entries: []string{registryHost + "/fn:v1"}},
			want: want{failed: map[string]string{
				registryHost + "/fn:v1": `cannot resolve module: module.oci.ref "` + registryHost + `/fn:v1" must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported`,
			}},
		},
		"UnknownRef": {
			reason: "A reference the registry does not hold is logged and does not stop the rest from warming.",
			args:   args{entries: []string{missingRef, "path:fn.wasm"}},
			want: want{loaded: 1, failed: map[string]string{
				missingRef: "cannot load module oci " + missingRef + ": cannot fetch module: cannot fetch manifest " + missingRef + ": ",
			}},
		},
		"MissingFile": {
			reason: "A path: entry that is not there is logged; nothing is loaded.",
			args:   args{entries: []string{"path:missing.wasm"}},
			want: want{failed: map[string]string{
				"path:missing.wasm": "cannot resolve module: cannot stat module file: stat " + filepath.Join(moduleDir, "missing.wasm") + ": no such file or directory",
			}},
		},
		"NotAModule": {
			reason: "Bytes that do not compile fail at warm-up exactly as they would at a request.",
			args:   args{entries: []string{"path:notwasm.wasm"}},
			want: want{failed: map[string]string{
				"path:notwasm.wasm": "cannot load module module file notwasm.wasm: cannot compile module: failed to parse WebAssembly module",
			}},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			eng, err := engine.New(engine.Config{})
			if err != nil {
				t.Fatal(err)
			}
			defer eng.Close()
			resolver, err := module.NewResolver(module.Options{Dir: moduleDir, Keychain: authn.NewMultiKeychain()})
			if err != nil {
				t.Fatal(err)
			}
			log := newRecorder()
			f := &Function{log: log, ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}

			f.warm(context.Background(), tc.args.entries, tc.args.parallel)

			if diff := cmp.Diff(tc.want.loaded, f.modules.Len()); diff != "" {
				t.Errorf("\n%s\nwarm() modules resident: -want, +got:\n%s", tc.reason, diff)
			}
			failed := map[string]string{}
			for _, line := range *log.seen {
				if rest, ok := strings.CutPrefix(line, "Cannot warm module module="); ok {
					entry, reason, _ := strings.Cut(rest, " error=")
					// A registry error ends with the server's own text;
					// what matters is where the load failed.
					if want, ok := tc.want.failed[entry]; ok && strings.HasSuffix(want, ": ") && strings.HasPrefix(reason, want) {
						reason = want
					}
					failed[entry] = reason
				}
			}
			if diff := cmp.Diff(tc.want.failed, failed); diff != "" {
				t.Errorf("\n%s\nwarm() failures logged: -want, +got:\n%s", tc.reason, diff)
			}
			if len(tc.args.entries) > 0 {
				if !slices.ContainsFunc(*log.seen, func(l string) bool { return strings.HasPrefix(l, "Warmed modules loaded=") }) {
					t.Errorf("\n%s\nwarm() must log its outcome, got:\n%s", tc.reason, strings.Join(*log.seen, "\n"))
				}
			}
		})
	}
}

// TestWarmThenRequest pins what warm-up is for: the first request for a
// warmed module is a memory hit — no fetch, no compile.
func TestWarmThenRequest(t *testing.T) {
	moduleDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(moduleDir, "fn.wasm"), testwasm.Fixed(t, guestResponse(), testwasm.Options{}), 0o600); err != nil {
		t.Fatal(err)
	}
	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{Dir: moduleDir})
	if err != nil {
		t.Fatal(err)
	}
	f := &Function{log: newRecorder(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}

	f.warm(context.Background(), []string{"path:fn.wasm"}, 1)

	hits, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventHit})
	misses, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventMiss})
	rsp, err := f.RunFunction(context.Background(), &fnv1.RunFunctionRequest{
		Meta:  &fnv1.RequestMeta{Tag: "hello"},
		Input: input(t, pathModule("fn.wasm")),
	})
	if err != nil {
		t.Fatalf("RunFunction(): %v", err)
	}
	if diff := cmp.Diff(guestResponse(), rsp, protocmp.Transform()); diff != "" {
		t.Errorf("RunFunction() after warm-up: -want, +got:\n%s", diff)
	}
	gotHits, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventHit})
	gotMisses, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventMiss})
	if diff := cmp.Diff(map[string]float64{"hit": hits + 1, "miss": misses}, map[string]float64{"hit": gotHits, "miss": gotMisses}); diff != "" {
		t.Errorf("first request for a warmed module must be a memory hit: -want, +got:\n%s", diff)
	}
}

// TestWarmModulesFlag pins how --warm-modules is given: repeated, or one
// comma-separated value, or the environment.
func TestWarmModulesFlag(t *testing.T) {
	type args struct {
		argv []string
		env  string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   []string
	}{
		"Repeated": {
			reason: "Each occurrence adds entries.",
			args:   args{argv: []string{"--warm-modules=path:a.wasm", "--warm-modules=ghcr.io/o/m@sha256:0000000000000000000000000000000000000000000000000000000000000000"}},
			want:   []string{"path:a.wasm", "ghcr.io/o/m@sha256:0000000000000000000000000000000000000000000000000000000000000000"},
		},
		"CommaSeparated": {
			reason: "One value may hold several entries.",
			args:   args{argv: []string{"--warm-modules=path:a.wasm,path:b.wasm"}},
			want:   []string{"path:a.wasm", "path:b.wasm"},
		},
		"Environment": {
			reason: "WARM_MODULES holds a comma-separated list.",
			args:   args{env: "path:a.wasm,path:b.wasm"},
			want:   []string{"path:a.wasm", "path:b.wasm"},
		},
		"Unset": {
			reason: "Nothing to warm by default.",
			want:   nil,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			t.Setenv("WARM_MODULES", tc.args.env)
			cli := &CLI{}
			parser, err := kong.New(cli)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := parser.Parse(tc.args.argv); err != nil {
				t.Fatalf("Parse(%v): %v", tc.args.argv, err)
			}
			if diff := cmp.Diff(tc.want, cli.WarmModules); diff != "" {
				t.Errorf("\n%s\n--warm-modules: -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}
