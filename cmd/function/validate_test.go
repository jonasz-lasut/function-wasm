package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

const testDigest = "sha256:3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a"

// TestValidate drives function validate through kong over the fixtures under
// testdata/validate: every refusal the runtime makes before it resolves a
// module, in the runtime's words, plus the tool's own behaviour — files,
// stdin, --xr, --function-name, warnings, JSON, exit codes.
func TestValidate(t *testing.T) {
	fixtures := filepath.Join("testdata", "validate")
	fixture := func(name string) string { return filepath.Join(fixtures, name) }
	warnPath := "  warning: module.type Path names a file under the runtime's --module-dir and carries no digest; a cluster Composition should pin an OCI or HTTP source by digest\n"

	type want struct {
		stdout string
		stderr string
		exit   int
	}
	cases := map[string]struct {
		reason string
		args   []string
		stdin  string
		want   want
	}{
		"Admitted": {
			reason: "An admitted step is one OK line naming the source and what it was granted; a Path source draws a warning and exit 0.",
			args:   []string{fixture("ok.yaml")},
			want: want{
				stdout: fixture("ok.yaml") + ": Composition/hello pipeline[0] hello: OK (path fn.wasm, limits timeout 5s memory 128Mi)\n" + warnPath,
			},
		},
		"Refusals": {
			reason: "Every refusal reads as the runtime's fatal result would; steps of other functions and non-Input inputs are skipped; a bare Input document is a step of its own; one refusal is exit 1.",
			args:   []string{fixture("refusals.yaml")},
			want: want{
				stdout: strings.Join([]string{
					fixture("refusals.yaml") + ": Composition/refusals pipeline[0] egress-without-flag: refused: sandbox.egress is refused: the runtime was started without --enable-sandbox-egress",
					fixture("refusals.yaml") + ": Composition/refusals pipeline[1] limits-over-ceiling: refused: limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi",
					fixture("refusals.yaml") + ": Composition/refusals pipeline[2] from-without-policy: refused: cannot resolve module: module.from: status.module of the composite resource names a OCI source, but policy.repositoryAllowList is not set: a module the composite resource chooses must be fenced to repositories the Composition names, or its author could point the runtime at any host",
					fixture("refusals.yaml") + `: Composition/refusals pipeline[3] tag-not-digest: refused: cannot resolve module: module.oci.ref "ghcr.io/example/greeter:v1" must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported`,
					fixture("refusals.yaml") + ": Composition/refusals pipeline[4] private-tmp-without-flag: refused: sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp",
					fixture("refusals.yaml") + ": Composition/refusals pipeline[7] wrong-shape: refused: cannot decode the Input: json: cannot unmarshal string into Go struct field Input.module of type v1beta1.ModuleSource",
					fixture("refusals.yaml") + ": Input[1] bare: OK (http https://example.com/fn.wasm)",
					"",
				}, "\n"),
				exit: 1,
			},
		},
		"EgressWithinPolicy": {
			reason: "With the sandbox flags and a policy file, grants within the ceiling are admitted and listed; a host outside the policy is refused naming what the policy admits; egress without --cosign-key is a warning.",
			args:   []string{fixture("egress.yaml"), "--enable-sandbox-egress", "--sandbox-egress-policy", fixture("egress-policy.yaml"), "--enable-sandbox-env", "--enable-sandbox-private-tmp"},
			want: want{
				stdout: fixture("egress.yaml") + ": Composition/egress pipeline[0] greeter: OK (oci ghcr.io/example/greeter:v1@" + testDigest + ", limits timeout 5s memory 128Mi, egress api.example.com, private /tmp, env GREETING_STYLE)\n" +
					"  warning: sandbox.egress is granted to a module that is not signature-verified: no --cosign-key was given\n" +
					fixture("egress.yaml") + `: Composition/egress pipeline[1] labeler: refused: sandbox.egress.http[0].host "evil.example.com" is outside the runtime's egress policy (allowed: api.example.com)` + "\n",
				exit: 1,
			},
		},
		"OperatorPolicy": {
			reason: "With --sandbox-policy-file the operator grant policy narrows within the floor: it permits the egress it grants but, default-deny, refuses the private /tmp the flag enabled.",
			args:   []string{fixture("operator-policy.yaml"), "--enable-sandbox-egress", "--sandbox-egress-policy", fixture("egress-policy.yaml"), "--enable-sandbox-private-tmp", "--sandbox-policy-file", fixture("policy.cedar")},
			want: want{
				stdout: fixture("operator-policy.yaml") + ": Composition/operator-policy pipeline[0] egress-ok: OK (oci ghcr.io/example/greeter@" + testDigest + ", egress api.example.com)\n" +
					"  warning: sandbox.egress is granted to a module that is not signature-verified: no --cosign-key was given\n" +
					fixture("operator-policy.yaml") + ": Composition/operator-policy pipeline[1] tmp-denied: refused: sandbox.filesystem.privateTmp is refused: the operator policy (--sandbox-policy-file) does not permit it for this request\n",
				exit: 1,
			},
		},
		"OperatorPolicyAbsent": {
			reason: "Without --sandbox-policy-file the same Composition against the same flags is admitted throughout: the policy is purely additive.",
			args:   []string{fixture("operator-policy.yaml"), "--enable-sandbox-egress", "--sandbox-egress-policy", fixture("egress-policy.yaml"), "--enable-sandbox-private-tmp"},
			want: want{
				stdout: fixture("operator-policy.yaml") + ": Composition/operator-policy pipeline[0] egress-ok: OK (oci ghcr.io/example/greeter@" + testDigest + ", egress api.example.com)\n" +
					"  warning: sandbox.egress is granted to a module that is not signature-verified: no --cosign-key was given\n" +
					fixture("operator-policy.yaml") + ": Composition/operator-policy pipeline[1] tmp-denied: OK (oci ghcr.io/example/greeter@" + testDigest + ", private /tmp)\n",
			},
		},
		"OperatorPolicyEnvEgressDenied": {
			reason: "The operator grant policy refuses the environment and egress it does not permit (default-deny), the same refusals the request path emits, so both surface in validate.",
			args:   []string{fixture("operator-policy-denied.yaml"), "--enable-sandbox-env", "--enable-sandbox-egress", "--sandbox-egress-policy", fixture("egress-policy.yaml"), "--sandbox-policy-file", fixture("policy-strict.cedar")},
			want: want{
				stdout: fixture("operator-policy-denied.yaml") + ": Composition/operator-policy-denied pipeline[0] env-denied: refused: sandbox.env is refused: the operator policy (--sandbox-policy-file) does not permit it for this request\n" +
					fixture("operator-policy-denied.yaml") + `: Composition/operator-policy-denied pipeline[1] egress-denied: refused: sandbox.egress.http[0] GET to host "api.example.com" is refused: the operator policy (--sandbox-policy-file) does not permit it` + "\n",
				exit: 1,
			},
		},
		"SignatureRequired": {
			reason: "A --sandbox-policy-file that requires a signature for a repository refuses a module from it under --resolve when no --cosign-key can verify it - the runtime's own words, settled before any fetch. The requirement is per-repository, so this is caught with no registry reached.",
			args:   []string{fixture("signature.yaml"), "--sandbox-policy-file", fixture("signature-policy.cedar"), "--resolve"},
			want: want{
				stdout: fixture("signature.yaml") + ": Composition/signature pipeline[0] secure: refused: cannot verify module oci ghcr.io/secure/greeter@" + testDigest + ": the operator policy requires a cosign signature, but the runtime has no --cosign-key to verify it\n",
				exit:   1,
			},
		},
		"EgressWithoutFlags": {
			reason: "The same Composition against a runtime with nothing enabled is refused at the first grant.",
			args:   []string{fixture("egress.yaml")},
			want: want{
				stdout: fixture("egress.yaml") + ": Composition/egress pipeline[0] greeter: refused: sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp\n" +
					fixture("egress.yaml") + ": Composition/egress pipeline[1] labeler: refused: sandbox.egress is refused: the runtime was started without --enable-sandbox-egress\n",
				exit: 1,
			},
		},
		"PolicyNeedsFlag": {
			reason: "A policy file without --enable-sandbox-egress is the tool's own error, exit 2, as it is the runtime's at startup.",
			args:   []string{fixture("egress.yaml"), "--sandbox-egress-policy", fixture("egress-policy.yaml")},
			want:   want{stderr: "function validate: --sandbox-egress-policy needs --enable-sandbox-egress\n", exit: 2},
		},
		"BadIPRule": {
			reason: "A malformed SSRF CIDR rule in --sandbox-policy-file stops the tool at load, exit 2, as it stops the runtime at startup.",
			args:   []string{fixture("ok.yaml"), "--sandbox-policy-file", fixture("bad-iprule.cedar")},
			want:   want{stderr: `function validate: operator policy: dialAddress rule "policy0": unsupported operation "isMulticast" (an ip test uses isInRange, isLoopback or ||)` + "\n", exit: 2},
		},
		"FromWithoutXR": {
			reason: "Without --xr a from source is checked for its policy fence and reported as the composite resource's choice.",
			args:   []string{fixture("from.yaml")},
			want: want{
				stdout: fixture("from.yaml") + ": Composition/from pipeline[0] chosen: OK (chosen by the composite resource from status.module (policy admits ghcr.io/example/))\n" +
					fixture("from.yaml") + ": Composition/from pipeline[1] other-registry: OK (chosen by the composite resource from status.other (policy admits ghcr.io/example/))\n",
			},
		},
		"FromWithXR": {
			reason: "With --xr the source is materialised as the runtime would and judged against the policy.",
			args:   []string{fixture("from.yaml"), "--xr", fixture("xr.yaml")},
			want: want{
				stdout: fixture("from.yaml") + ": Composition/from pipeline[0] chosen: OK (oci ghcr.io/example/greeter@" + testDigest + " (from status.module))\n" +
					fixture("from.yaml") + `: Composition/from pipeline[1] other-registry: refused: cannot resolve module: module.from: status.other of the composite resource names ref "index.docker.io/someone/else", which policy.repositoryAllowList does not admit (allowed prefixes: ghcr.io/example/)` + "\n",
				exit: 1,
			},
		},
		"UnknownFields": {
			reason: "Fields the runtime would silently ignore are warnings naming them, wherever they are; the step is still admitted.",
			args:   []string{fixture("unknown.yaml")},
			want: want{
				stdout: fixture("unknown.yaml") + ": Input[0]: OK (oci ghcr.io/example/greeter@" + testDigest + ")\n" +
					"  warning: unknown field \"limit\" is ignored by the runtime\n" +
					"  warning: unknown field \"credential\" is ignored by the runtime\n",
			},
		},
		"LimitsEqualCeiling": {
			reason: "A limit equal to its ceiling narrows nothing and is a warning.",
			args:   []string{fixture("ok.yaml"), "--module-timeout", "5s", "--module-memory-limit", "128"},
			want: want{
				stdout: fixture("ok.yaml") + ": Composition/hello pipeline[0] hello: OK (path fn.wasm, limits timeout 5s memory 128Mi)\n" + warnPath +
					"  warning: limits.timeout 5s equals --module-timeout: it narrows nothing\n" +
					"  warning: limits.memory 128Mi equals --module-memory-limit (128Mi): it narrows nothing\n",
			},
		},
		"NoInputs": {
			reason: "A file without a function-wasm Input is noted on stderr and is not a failure.",
			args:   []string{fixture("function.yaml")},
			want:   want{stderr: fixture("function.yaml") + ": no function-wasm Input found\n"},
		},
		"FunctionName": {
			reason: "--function-name keeps only the steps of that function.",
			args:   []string{fixture("refusals.yaml"), "--function-name", "function-auto-ready"},
			want:   want{stderr: fixture("refusals.yaml") + ": no function-wasm Input found\n"},
		},
		"Unparsable": {
			reason: "A file that is not YAML is the tool's failure, exit 2.",
			args:   []string{fixture("broken.yaml")},
			want:   want{stderr: "function validate: cannot parse " + fixture("broken.yaml") + ": error converting YAML to JSON: yaml: line 3: did not find expected ',' or ']'\n", exit: 2},
		},
		"Missing": {
			reason: "So is a file that does not exist.",
			args:   []string{fixture("nope.yaml")},
			want:   want{stderr: "function validate: cannot read " + fixture("nope.yaml") + ": open " + fixture("nope.yaml") + ": no such file or directory\n", exit: 2},
		},
		"Stdin": {
			reason: "- reads stdin.",
			args:   []string{"-"},
			stdin:  "apiVersion: wasm.fn.crossplane.io/v1beta1\nkind: Input\nmodule: {type: OCI, oci: {ref: ghcr.io/example/greeter@" + testDigest + "}}\n",
			want:   want{stdout: "-: Input[0]: OK (oci ghcr.io/example/greeter@" + testDigest + ")\n"},
		},
		"SeveralFiles": {
			reason: "Files are checked in order; the exit code covers them all.",
			args:   []string{fixture("ok.yaml"), fixture("from.yaml"), "--xr", fixture("xr.yaml")},
			want: want{
				stdout: fixture("ok.yaml") + ": Composition/hello pipeline[0] hello: OK (path fn.wasm, limits timeout 5s memory 128Mi)\n" + warnPath +
					fixture("from.yaml") + ": Composition/from pipeline[0] chosen: OK (oci ghcr.io/example/greeter@" + testDigest + " (from status.module))\n" +
					fixture("from.yaml") + `: Composition/from pipeline[1] other-registry: refused: cannot resolve module: module.from: status.other of the composite resource names ref "index.docker.io/someone/else", which policy.repositoryAllowList does not admit (allowed prefixes: ghcr.io/example/)` + "\n",
				exit: 1,
			},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if tc.stdin != "" {
				r, w, err := os.Pipe()
				if err != nil {
					t.Fatal(err)
				}
				_, _ = w.WriteString(tc.stdin)
				_ = w.Close()
				old := os.Stdin
				os.Stdin = r
				t.Cleanup(func() { os.Stdin = old })
			}
			var stdout, stderr bytes.Buffer
			cli := &CLI{}
			cli.Validate.stderr = &stderr
			ctx, err := parser(cli, &stdout).Parse(append([]string{"validate"}, tc.args...))
			if err != nil {
				t.Fatalf("\n%s\nParse(): %v", tc.reason, err)
			}
			err = ctx.Run(cli)
			exit := 0
			var e exitError
			if errors.As(err, &e) {
				exit = e.code
			} else if err != nil {
				t.Fatalf("\n%s\nRun(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.stdout, stdout.String()); diff != "" {
				t.Errorf("\n%s\nstdout: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.stderr, stderr.String()); diff != "" {
				t.Errorf("\n%s\nstderr: -want, +got:\n%s", tc.reason, diff)
			}
			if exit != tc.want.exit {
				t.Errorf("\n%s\nexit code: want %d, got %d", tc.reason, tc.want.exit, exit)
			}
		})
	}
}

// TestValidateResolve pins --resolve: the module is resolved through the
// runtime's resolver (--module-dir for a Path source), fetched and compiled
// with wasmtime — digest, size, ABI verdict, host imports — and a module the
// runtime would refuse at load is refused here with the same words.
func TestValidateResolve(t *testing.T) {
	dir := t.TempDir()
	okModule := testwasm.Fixed(t, &fnv1.RunFunctionResponse{}, testwasm.Options{Extra: `(import "wasmfn" "log" (func $log (param i32 i32 i32)))`})
	badModule := testwasm.Fixed(t, &fnv1.RunFunctionResponse{}, testwasm.Options{SkipRun: true})
	for name, wasm := range map[string][]byte{"fn.wasm": okModule, "bad.wasm": badModule, "notwasm.wasm": []byte("hello")} {
		if err := os.WriteFile(filepath.Join(dir, name), wasm, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	composition := filepath.Join(dir, "composition.yaml")
	if err := os.WriteFile(composition, []byte(`apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: resolve
spec:
  pipeline:
  - step: ok
    functionRef: {name: function-wasm}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module: {type: Path, path: fn.wasm}
  - step: bad
    functionRef: {name: function-wasm}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module: {type: Path, path: bad.wasm}
  - step: notwasm
    functionRef: {name: function-wasm}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module: {type: Path, path: notwasm.wasm}
  - step: missing
    functionRef: {name: function-wasm}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module: {type: Path, path: missing.wasm}
`), 0o600); err != nil {
		t.Fatal(err)
	}
	warnPath := "  warning: module.type Path names a file under the runtime's --module-dir and carries no digest; a cluster Composition should pin an OCI or HTTP source by digest\n"

	var stdout, stderr bytes.Buffer
	cli := &CLI{}
	cli.Validate.stderr = &stderr
	ctx, err := parser(cli, &stdout).Parse([]string{"validate", composition, "--resolve", "--module-dir", dir})
	if err != nil {
		t.Fatal(err)
	}
	err = ctx.Run(cli)
	var e exitError
	if !errors.As(err, &e) || e.code != 1 {
		t.Errorf("Run(): want exit 1, got %v", err)
	}
	want := composition + ": Composition/resolve pipeline[0] ok: OK (path fn.wasm)\n" +
		"  module: " + digestOf(okModule) + ", " + humanBytes(len(okModule)) + ", ABI v1, imports wasmfn.log\n" + warnPath +
		composition + `: Composition/resolve pipeline[1] bad: refused: cannot load module module file bad.wasm: module does not export "wasmfn_run"` + "\n" + warnPath +
		composition + ": Composition/resolve pipeline[2] notwasm: refused: cannot load module module file notwasm.wasm: cannot compile module: failed to parse WebAssembly module\n" + warnPath +
		composition + ": Composition/resolve pipeline[3] missing: refused: cannot resolve module: cannot stat module file: stat " + filepath.Join(dir, "missing.wasm") + ": no such file or directory\n" + warnPath
	if diff := cmp.Diff(want, stdout.String()); diff != "" {
		t.Errorf("stdout: -want, +got:\n%s", diff)
	}
	if stderr.Len() != 0 {
		t.Errorf("unexpected stderr:\n%s", stderr.String())
	}

	// The same as JSON: one object per step, the resolved shape included.
	stdout.Reset()
	cli = &CLI{}
	ctx, err = parser(cli, &stdout).Parse([]string{"validate", composition, "--resolve", "--module-dir", dir, "--output", "json"})
	if err != nil {
		t.Fatal(err)
	}
	_ = ctx.Run(cli)
	lines := strings.Split(strings.TrimSpace(stdout.String()), "\n")
	if len(lines) != 4 {
		t.Fatalf("want 4 JSON lines, got %d:\n%s", len(lines), stdout.String())
	}
	var first stepResult
	if err := json.Unmarshal([]byte(lines[0]), &first); err != nil {
		t.Fatalf("cannot decode %q: %v", lines[0], err)
	}
	wantFirst := stepResult{
		File: composition, Composition: "resolve", Index: 0, Step: "ok", Function: "function-wasm", Status: "ok", Module: "path fn.wasm",
		Warnings: []string{strings.TrimSpace(strings.TrimPrefix(warnPath, "  warning: "))},
		Resolved: &resolvedModule{Digest: digestOf(okModule), Size: len(okModule), ABI: "v1", Imports: []string{"wasmfn.log"}},
	}
	if diff := cmp.Diff(wantFirst, first); diff != "" {
		t.Errorf("first JSON object: -want, +got:\n%s", diff)
	}
	var second stepResult
	if err := json.Unmarshal([]byte(lines[1]), &second); err != nil {
		t.Fatal(err)
	}
	if second.Status != "refused" || !strings.Contains(second.Message, `does not export "wasmfn_run"`) {
		t.Errorf("second JSON object: %+v", second)
	}
}

// TestValidateResolveManifest pins that --resolve reads a module's manifest
// from the artifact and holds it against the step's grants as the runtime
// would: the summary on the resolved line, and the runtime's refusal when a
// requirement is not granted.
func TestValidateResolveManifest(t *testing.T) {
	wasm := testwasm.Fixed(t, &fnv1.RunFunctionResponse{}, testwasm.Options{})
	ref := pushWithManifest(t, publicRegistry(t)+"/greeter:v1", wasm, `{"abi":1,"name":"greeter","version":"0.1.0","requires":{"egress":{"http":[{"host":"api.example.com","methods":["GET"]}]}}}`)
	dir := t.TempDir()
	composition := filepath.Join(dir, "composition.yaml")
	if err := os.WriteFile(composition, []byte(`apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: manifest
spec:
  pipeline:
  - step: granted
    functionRef: {name: function-wasm}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module: {type: OCI, oci: {ref: `+ref+`}}
      sandbox: {egress: {http: [{host: api.example.com, methods: [GET]}]}}
  - step: ungranted
    functionRef: {name: function-wasm}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module: {type: OCI, oci: {ref: `+ref+`}}
`), 0o600); err != nil {
		t.Fatal(err)
	}

	var stdout, stderr bytes.Buffer
	cli := &CLI{}
	cli.Validate.stderr = &stderr
	ctx, err := parser(cli, &stdout).Parse([]string{"validate", composition, "--resolve", "--enable-sandbox-egress"})
	if err != nil {
		t.Fatal(err)
	}
	err = ctx.Run(cli)
	var e exitError
	if !errors.As(err, &e) || e.code != 1 {
		t.Errorf("Run(): want exit 1, got %v", err)
	}
	digest := ref[strings.Index(ref, "@")+1:]
	resolved := "  module: " + digest + ", " + humanBytes(len(wasm)) + ", ABI v1; manifest: greeter 0.1.0, requires egress api.example.com\n"
	want := composition + ": Composition/manifest pipeline[0] granted: OK (oci " + ref + ", egress api.example.com)\n" + resolved +
		"  warning: sandbox.egress is granted to a module that is not signature-verified: no --cosign-key was given\n" +
		composition + ": Composition/manifest pipeline[1] ungranted: refused: module oci " + ref + " requires sandbox.egress.http host api.example.com methods [GET], which the Composition does not grant\n" + resolved
	if diff := cmp.Diff(want, stdout.String()); diff != "" {
		t.Errorf("stdout: -want, +got:\n%s", diff)
	}
	if stderr.Len() != 0 {
		t.Errorf("unexpected stderr:\n%s", stderr.String())
	}
}
