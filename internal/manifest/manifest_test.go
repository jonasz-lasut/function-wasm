package manifest

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"k8s.io/apimachinery/pkg/runtime"

	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

const greeterSchema = `{"type":"object","properties":{"greeting":{"type":"string"},"greetingUrl":{"type":"string","format":"uri"}},"additionalProperties":false}`

func rule(host, pattern string, methods []string, prefix string) egress.HTTPRule {
	return egress.HTTPRule{Host: host, HostPattern: pattern, Methods: methods, PathPrefix: prefix}
}

func rawConfig(s string) *runtime.RawExtension {
	return &runtime.RawExtension{Raw: []byte(s)}
}

// full is a manifest that requires everything, for Check and Sandbox.
func full() *Manifest {
	return &Manifest{
		ABI: 1, Name: "greeter", Version: "0.1.0",
		Requires: &Requires{
			Egress:     &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"GET"}, "/v1/")}},
			Filesystem: &Filesystem{PrivateTmp: true},
		},
		Config:     &Config{Schema: json.RawMessage(greeterSchema)},
		MinRuntime: "v0.2.0",
	}
}

func TestParse(t *testing.T) {
	cases := map[string]struct {
		reason string
		raw    string
		want   *Manifest
		err    string
	}{
		"Minimal": {
			reason: "abi alone is a manifest.",
			raw:    `{"abi":1}`,
			want:   &Manifest{ABI: 1},
		},
		"Full": {
			reason: "Every field decodes into the Input's own types.",
			raw:    `{"abi":1,"name":"greeter","version":"0.1.0","source":"https://example.com/g","description":"greets","requires":{"egress":{"http":[{"host":"api.example.com","methods":["GET"],"pathPrefix":"/v1/"}]},"filesystem":{"privateTmp":true}},"config":{"schema":` + greeterSchema + `},"minRuntime":"0.2.0"}`,
			want: &Manifest{
				ABI: 1, Name: "greeter", Version: "0.1.0", Source: "https://example.com/g", Description: "greets",
				Requires: &Requires{
					Egress:     &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"GET"}, "/v1/")}},
					Filesystem: &Filesystem{PrivateTmp: true},
				},
				Config:     &Config{Schema: json.RawMessage(greeterSchema)},
				MinRuntime: "0.2.0",
			},
		},
		"UnknownTopLevelIgnored": {
			reason: "A field a newer guestfn wrote at the top level does not stop an older runtime.",
			raw:    `{"abi":1,"future":{"x":1}}`,
			want:   &Manifest{ABI: 1},
		},
		"UnknownRequirementRefused": {
			reason: "A requirement this runtime cannot honour fails closed.",
			raw:    `{"abi":1,"requires":{"gpu":true}}`,
			err:    `cannot parse manifest requires: json: unknown field "gpu"`,
		},
		"UnknownNestedRequirementRefused": {
			reason: "So does an unknown field deeper under requires.",
			raw:    `{"abi":1,"requires":{"egress":{"http":[{"host":"a.example.com","methods":["GET"],"retries":3}]}}}`,
			err:    `cannot parse manifest requires: json: unknown field "retries"`,
		},
		"NullRequires": {
			reason: "requires: null is no requirement.",
			raw:    `{"abi":1,"requires":null}`,
			want:   &Manifest{ABI: 1},
		},
		"BadABI": {
			reason: "Another ABI is refused.",
			raw:    `{"abi":2}`,
			err:    "abi must be 1 (this runtime implements ABI v1), got 2",
		},
		"MissingABI": {
			reason: "abi is required.",
			raw:    `{"name":"x"}`,
			err:    "abi must be 1",
		},
		"NotJSON": {
			reason: "Bytes that are not JSON are refused.",
			raw:    `abi: 1`,
			err:    "cannot parse manifest:",
		},
		"TooLarge": {
			reason: "The layer is bounded.",
			raw:    `{"abi":1,"description":"` + strings.Repeat("x", MaxSize) + `"}`,
			err:    "the limit is 65536",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := Parse([]byte(tc.raw))
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nParse(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nParse(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, got, cmpopts.IgnoreUnexported(Manifest{})); diff != "" {
				t.Errorf("\n%s\nParse(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func TestLoad(t *testing.T) {
	cases := map[string]struct {
		reason string
		yaml   string
		want   *Manifest
		err    string
	}{
		"Scaffold": {
			reason: "The scaffold's wasmfn.yaml loads with its schema.",
			yaml: `abi: 1
name: greeter
version: 0.1.0
config:
  schema:
    type: object
    properties:
      greeting: {type: string}
      greetingUrl: {type: string, format: uri}
    additionalProperties: false
`,
			want: &Manifest{ABI: 1, Name: "greeter", Version: "0.1.0", Config: &Config{Schema: json.RawMessage(greeterSchema)}},
		},
		"Requires": {
			reason: "requires uses the Input's rule shape verbatim.",
			yaml: `abi: 1
requires:
  egress:
    http:
    - host: api.example.com
      methods: [GET]
      pathPrefix: /v1/
  filesystem: {privateTmp: true}
minRuntime: v0.2.0
`,
			want: &Manifest{ABI: 1, Requires: &Requires{
				Egress:     &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"GET"}, "/v1/")}},
				Filesystem: &Filesystem{PrivateTmp: true},
			}, MinRuntime: "v0.2.0"},
		},
		"Strict": {
			reason: "A typo anywhere in the file is an error, not forward compatibility.",
			yaml:   "abi: 1\ntitle: greeter\n",
			err:    `unknown field "title"`,
		},
		"Invalid": {
			reason: "The file is validated.",
			yaml:   "abi: 1\nrequires:\n  egress:\n    http:\n    - host: api.example.com\n",
			err:    `requires.egress.http[0].methods must list at least one method`,
		},
		"EnvBindings": {
			reason: "requires.env binds variables to step credential keys - the module's own env contract.",
			yaml: `abi: 1
requires:
  env:
  - name: DATABASE_URL
    fromCredential: {name: db, key: url}
`,
			want: &Manifest{ABI: 1, Requires: &Requires{
				Env: []sandbox.EnvBinding{{Name: "DATABASE_URL", FromCredential: sandbox.CredentialKey{Name: "db", Key: "url"}}},
			}},
		},
		"EnvLiteralRefused": {
			reason: "A literal env value is not expressible: non-secret configuration is the Input's config.",
			yaml:   "abi: 1\nrequires:\n  env:\n  - name: LOG_LEVEL\n    value: debug\n",
			err:    `unknown field "value"`,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), FileName)
			if err := os.WriteFile(path, []byte(tc.yaml), 0o600); err != nil {
				t.Fatal(err)
			}
			got, err := Load(path)
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nLoad(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nLoad(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, got, cmpopts.IgnoreUnexported(Manifest{}), cmp.Transformer("json", func(r json.RawMessage) string {
				var v any
				if err := json.Unmarshal(r, &v); err != nil {
					return string(r)
				}
				b, _ := json.Marshal(v)
				return string(b)
			})); diff != "" {
				t.Errorf("\n%s\nLoad(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
	if _, err := Load(filepath.Join(t.TempDir(), "missing.yaml")); err == nil || !strings.Contains(err.Error(), "cannot read manifest") {
		t.Errorf("Load(missing): want a read error, got %v", err)
	}
}

func TestValidate(t *testing.T) {
	cases := map[string]struct {
		reason string
		m      *Manifest
		err    string
	}{
		"OK": {
			reason: "The full manifest is valid.",
			m:      full(),
		},
		"BadRule": {
			reason: "A required egress rule passes the Composition's own rule checks, named as requires.egress.http[i].",
			m:      &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", nil, "")}}}},
			err:    "requires.egress.http[0].methods must list at least one method",
		},
		"BadRuleHost": {
			reason: "A rule with both host and hostPattern is refused.",
			m:      &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("a.example.com", "*.example.com", []string{"GET"}, "")}}}},
			err:    "requires.egress.http[0] must set exactly one of host and hostPattern",
		},
		"BadBindingName": {
			reason: "An env binding's name must be an identifier, named as requires.env[i].",
			m:      &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{{Name: "1x", FromCredential: sandbox.CredentialKey{Name: "db", Key: "url"}}}}},
			err:    `requires.env[0].name "1x" is not an identifier`,
		},
		"BindingWithoutKey": {
			reason: "A binding names exactly one credential key; a missing key is refused.",
			m:      &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "db"}}}}},
			err:    "requires.env[0].fromCredential.key must not be empty",
		},
		"DuplicateBinding": {
			reason: "A variable bound twice is refused.",
			m: &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{
				{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "db", Key: "a"}},
				{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "db", Key: "b"}},
			}}},
			err: "requires.env[1]: TOKEN is already bound by requires.env[0]",
		},
		"BadSemver": {
			reason: "minRuntime is a semantic version.",
			m:      &Manifest{ABI: 1, MinRuntime: "latest"},
			err:    `minRuntime "latest" is not a semantic version (e.g. v0.2.0)`,
		},
		"SemverWithoutV": {
			reason: "0.2.0 is accepted as v0.2.0.",
			m:      &Manifest{ABI: 1, MinRuntime: "0.2.0"},
		},
		"RefToURL": {
			reason: "A schema that refers to a URL is refused: schemas are inline, nothing is fetched.",
			m:      &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{"$ref":"https://example.com/schema.json"}`)}},
			err:    "config.schema: $ref to https://example.com/schema.json is not allowed: schemas are inline",
		},
		"RefToFile": {
			reason: "Nor a file.",
			m:      &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{"$ref":"file:///etc/passwd"}`)}},
			err:    "config.schema: $ref to file:///etc/passwd is not allowed: schemas are inline",
		},
		"LocalRef": {
			reason: "A $ref inside the document is fine.",
			m:      &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{"$defs":{"s":{"type":"string"}},"properties":{"greeting":{"$ref":"#/$defs/s"}}}`)}},
		},
		"BadSchema": {
			reason: "A schema that is not a schema is refused.",
			m:      &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{"type":"strng"}`)}},
			err:    "config.schema:",
		},
		"SchemaNotJSON": {
			reason: "A schema that is not JSON is refused.",
			m:      &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{`)}},
			err:    "config.schema is not JSON",
		},
		"BadABI": {
			reason: "abi is checked here too.",
			m:      &Manifest{ABI: 0},
			err:    "abi must be 1 (this runtime implements ABI v1), got 0",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := tc.m.Validate()
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nValidate(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nValidate(): unexpected error %v", tc.reason, err)
			}
		})
	}
}

func TestCheck(t *testing.T) {
	get := []string{"GET"}
	type args struct {
		m       *Manifest
		grants  Grants
		config  *runtime.RawExtension
		runtime string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   string
	}{
		"AllCovered": {
			reason: "Every requirement covered, the runtime new enough, the config valid: no error.",
			args: args{m: full(), grants: Grants{
				PrivateTmp: true,
				HTTP:       []egress.HTTPRule{rule("api.example.com", "", []string{"GET", "POST"}, "/v1/")},
			}, config: rawConfig(`{"greeting":"hi"}`), runtime: "v0.2.1"},
		},
		"NoRequirements": {
			reason: "A manifest without requires passes any grant.",
			args:   args{m: &Manifest{ABI: 1}},
		},
		"EgressMissing": {
			reason: "A required rule no granted rule covers is named as the module wrote it.",
			args:   args{m: full(), grants: Grants{PrivateTmp: true}},
			want:   "requires egress host api.example.com methods [GET] pathPrefix /v1/, which was not granted",
		},
		"EgressWrongHost": {
			reason: "Another host does not cover it.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("other.example.com", "", get, "")}}},
			want: "requires egress host api.example.com methods [GET], which was not granted",
		},
		"PatternCoversHost": {
			reason: "A granted pattern covers a required host under it.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("", "*.example.com", get, "")}}},
		},
		"PatternDoesNotCoverApex": {
			reason: "*.example.com does not cover example.com itself.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("example.com", "", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("", "*.example.com", get, "")}}},
			want: "requires egress host example.com methods [GET], which was not granted",
		},
		"PatternUnderPattern": {
			reason: "A required pattern under a granted pattern is covered.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("", "*.a.example.com", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("", "*.example.com", get, "")}}},
		},
		"PatternEqual": {
			reason: "The same pattern is covered.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("", "*.example.com", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("", "*.example.com", get, "")}}},
		},
		"HostDoesNotCoverPattern": {
			reason: "A granted exact host never covers a required pattern.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("", "*.example.com", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "")}}},
			want: "requires egress hostPattern *.example.com methods [GET], which was not granted",
		},
		"MethodSubset": {
			reason: "The granted methods must include every required one, whatever the case.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"GET", "POST"}, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"get"}, "")}}},
			want: "requires egress host api.example.com methods [GET POST], which was not granted",
		},
		"MethodsCaseInsensitive": {
			reason: "get covers GET.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"GET"}, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", []string{"get"}, "")}}},
		},
		"PrefixGrantedEmptyAdmitsAll": {
			reason: "A granted rule without a path prefix admits every required prefix.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/v1/")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "")}}},
		},
		"PrefixNarrower": {
			reason: "A required prefix under the granted prefix is covered.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/v1/items/")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/v1/")}}},
		},
		"PrefixWider": {
			reason: "A required prefix wider than the granted one is not covered.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/v1/")}}},
			want: "requires egress host api.example.com methods [GET] pathPrefix /, which was not granted",
		},
		"PrefixRequiredEmptyGrantedSet": {
			reason: "A required rule without a prefix needs a granted rule without one.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/v1/")}}},
			want: "requires egress host api.example.com methods [GET], which was not granted",
		},
		"SecondGrantedRuleCovers": {
			reason: "Any granted rule may cover the requirement.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "")}}}},
				grants: Grants{HTTP: []egress.HTTPRule{rule("other.example.com", "", get, ""), rule("api.example.com", "", get, "")}}},
		},
		"PrivateTmpMissing": {
			reason: "The private /tmp must be granted.",
			args:   args{m: &Manifest{ABI: 1, Requires: &Requires{Filesystem: &Filesystem{PrivateTmp: true}}}},
			want:   "requires a private /tmp (requires.filesystem.privateTmp), which was not granted",
		},
		"PrivateTmpFalse": {
			reason: "privateTmp: false requires nothing.",
			args:   args{m: &Manifest{ABI: 1, Requires: &Requires{Filesystem: &Filesystem{PrivateTmp: false}}}},
		},
		"EnvBindingGranted": {
			reason: "A required binding among the granted ones passes.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "token"}}}}},
				grants: Grants{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "token"}}}}},
		},
		"EnvBindingMissing": {
			reason: "A required binding not granted is named exactly.",
			args:   args{m: &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "token"}}}}}},
			want:   `requires env TOKEN from credential "api" key "token", which was not granted`,
		},
		"EnvBindingDifferentKey": {
			reason: "A grant for the same variable from another key does not cover it: bindings match exactly.",
			args: args{m: &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "token"}}}}},
				grants: Grants{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "other"}}}}},
			want: `requires env TOKEN from credential "api" key "token", which was not granted`,
		},
		"RuntimeTooOld": {
			reason: "minRuntime above the runtime's version is refused, both versions named.",
			args:   args{m: &Manifest{ABI: 1, MinRuntime: "v0.3.0"}, runtime: "v0.2.1"},
			want:   "requires runtime v0.3.0 or newer, this is v0.2.1",
		},
		"RuntimeNewEnough": {
			reason: "An equal or newer runtime passes.",
			args:   args{m: &Manifest{ABI: 1, MinRuntime: "0.3.0"}, runtime: "v0.3.0"},
		},
		"RuntimeDevel": {
			reason: "A development build passes every minRuntime.",
			args:   args{m: &Manifest{ABI: 1, MinRuntime: "v9.0.0"}, runtime: "(devel)"},
		},
		"RuntimeUnknown": {
			reason: "So does a runtime without build information.",
			args:   args{m: &Manifest{ABI: 1, MinRuntime: "v9.0.0"}, runtime: ""},
		},
		"RuntimePseudoVersion": {
			reason: "A pseudo-version compares as semver.",
			args:   args{m: &Manifest{ABI: 1, MinRuntime: "v0.3.0"}, runtime: "v0.3.1-0.20260817120000-abcdef123456"},
		},
		"SchemaOK": {
			reason: "A config the schema accepts passes.",
			args:   args{m: full(), grants: Grants{PrivateTmp: true, HTTP: []egress.HTTPRule{rule("api.example.com", "", get, "/v1/")}}, config: rawConfig(`{"greeting":"hi","greetingUrl":"https://example.com/en"}`)},
		},
		"SchemaWrongType": {
			reason: "A wrong type is named by JSON pointer and the library's message.",
			args:   args{m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(greeterSchema)}}, config: rawConfig(`{"greeting":1}`)},
			want:   "config does not match the module's schema: /greeting: got number, want string",
		},
		"SchemaUnknownKey": {
			reason: "additionalProperties: false refuses a typo at the root.",
			args:   args{m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(greeterSchema)}}, config: rawConfig(`{"greetng":"hi"}`)},
			want:   "config does not match the module's schema: /: additional properties 'greetng' not allowed",
		},
		"SchemaRequiredMissing": {
			reason: "A missing required key is refused; a nil config is an empty object.",
			args:   args{m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{"type":"object","required":["greeting"]}`)}}},
			want:   "config does not match the module's schema: /: missing property 'greeting'",
		},
		"SchemaFormat": {
			reason: "Formats are asserted.",
			args:   args{m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(greeterSchema)}}, config: rawConfig(`{"greetingUrl":"not a url"}`)},
			want:   "config does not match the module's schema: /greetingUrl: 'not a url' is not valid uri:",
		},
		"SchemaNested": {
			reason: "A nested failure carries its full pointer.",
			args:   args{m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{"properties":{"a":{"properties":{"b/c":{"type":"integer"}}}}}`)}}, config: rawConfig(`{"a":{"b/c":"x"}}`)},
			want:   "config does not match the module's schema: /a/b~1c: got string, want integer",
		},
		"ConfigNotJSON": {
			reason: "A config that is not JSON is refused.",
			args:   args{m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(greeterSchema)}}, config: rawConfig(`{`)},
			want:   "config is not JSON",
		},
		"NoSchemaAnyConfig": {
			reason: "Without a schema any config passes.",
			args:   args{m: &Manifest{ABI: 1}, config: rawConfig(`{"anything":[1,2]}`)},
		},
		"BadABI": {
			reason: "Another ABI is refused before anything else.",
			args:   args{m: &Manifest{ABI: 2}},
			want:   "requires ABI v2, this runtime implements ABI v1",
		},
		"OrderEgressBeforeTmp": {
			reason: "The first miss wins: egress before the filesystem, runtime and schema.",
			args:   args{m: full()},
			want:   "requires egress host api.example.com methods [GET] pathPrefix /v1/, which was not granted",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			// Validate compiles the schema as Parse and Load do; a wrong ABI
			// fails it too, and Check must still say so on its own.
			if err := tc.args.m.Validate(); err != nil && tc.args.m.ABI == ABIVersion {
				t.Fatalf("\n%s\nValidate(): %v", tc.reason, err)
			}
			err := tc.args.m.Check(tc.args.grants, tc.args.config, tc.args.runtime)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nCheck(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.HasPrefix(err.Error(), tc.want) {
				t.Fatalf("\n%s\nCheck(): want error starting with %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

func TestValidateConfigWithoutValidate(t *testing.T) {
	// A hand-built manifest compiles its schema on first use.
	m := &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(greeterSchema)}}
	if err := m.ValidateConfig(rawConfig(`{"greeting":"hi"}`)); err != nil {
		t.Fatalf("ValidateConfig(): %v", err)
	}
	if err := m.ValidateConfig(rawConfig(`{"greeting":1}`)); err == nil || err.Error() != "config does not match the module's schema: /greeting: got number, want string" {
		t.Errorf("ValidateConfig(): got %v", err)
	}
	if err := (&Manifest{ABI: 1}).ValidateConfig(rawConfig(`1`)); err != nil {
		t.Errorf("ValidateConfig() without a schema: %v", err)
	}
}

func TestSummary(t *testing.T) {
	cases := map[string]struct {
		m    *Manifest
		want string
	}{
		"Empty":   {m: &Manifest{ABI: 1}, want: ""},
		"Name":    {m: &Manifest{ABI: 1, Name: "greeter"}, want: "greeter"},
		"Full":    {m: full(), want: "greeter 0.1.0, requires egress api.example.com, private /tmp; config schema; runtime v0.2.0 or newer"},
		"Pattern": {m: &Manifest{ABI: 1, Version: "1.0.0", Requires: &Requires{Egress: &Egress{HTTP: []egress.HTTPRule{rule("", "*.example.com", []string{"GET"}, "")}}}}, want: "1.0.0, requires egress *.example.com"},
		"Schema":  {m: &Manifest{ABI: 1, Config: &Config{Schema: json.RawMessage(`{}`)}}, want: "config schema"},
		"Env": {m: &Manifest{ABI: 1, Requires: &Requires{Env: []sandbox.EnvBinding{
			{Name: "DATABASE_URL", FromCredential: sandbox.CredentialKey{Name: "db", Key: "url"}},
			{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "token"}},
		}}}, want: "env DATABASE_URL TOKEN"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, tc.m.Summary()); diff != "" {
				t.Errorf("Summary(): -want, +got:\n%s", diff)
			}
		})
	}
}

func TestJSONRoundTrip(t *testing.T) {
	m := full()
	b, err := m.JSON()
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(b), "\n") {
		t.Errorf("JSON() is not compact:\n%s", b)
	}
	got, err := Parse(b)
	if err != nil {
		t.Fatalf("Parse(JSON()): %v", err)
	}
	if diff := cmp.Diff(m, got, cmpopts.IgnoreUnexported(Manifest{})); diff != "" {
		t.Errorf("round trip: -want, +got:\n%s", diff)
	}
	if got.Requires.Empty() {
		t.Error("round trip lost the requirements")
	}
}

func TestRuntimeVersion(t *testing.T) {
	// A test binary carries build information without a main version tag;
	// either way the value must be usable by Check.
	if err := (&Manifest{ABI: 1, MinRuntime: "v9.9.9"}).Check(Grants{}, nil, RuntimeVersion()); err != nil {
		t.Errorf("a development runtime must pass every minRuntime, got %v (RuntimeVersion=%q)", err, RuntimeVersion())
	}
}
