package sandbox

import (
	"strings"
	"testing"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

func TestValidate(t *testing.T) {
	cases := map[string]struct {
		reason  string
		sandbox *v1beta1.Sandbox
		want    string
	}{
		"Nil":   {reason: "No sandbox is the default sandbox."},
		"Empty": {reason: "An empty sandbox asks for nothing and is valid.", sandbox: &v1beta1.Sandbox{}},
		"Full": {
			reason: "A well-formed grant of every kind is valid.",
			sandbox: &v1beta1.Sandbox{
				Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true},
				Egress:     &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}, PathPrefix: "/v1/"}, {HostPattern: "*.internal.example.com", Methods: []string{"GET", "POST"}}}},
				Env:        []v1beta1.EnvVar{{Name: "LOG_LEVEL", Value: new("debug")}, {Name: "_x1", Value: new("y")}},
			},
		},
		"FullValueFrom": {
			reason: "An env entry with valueFrom is valid when the credential reference is complete.",
			sandbox: &v1beta1.Sandbox{
				Env: []v1beta1.EnvVar{{Name: "AWS_KEY", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "aws", Key: "access_key_id"}}}},
			},
		},
		"EnvFromValid": {
			reason: "A well-formed envFrom is valid.",
			sandbox: &v1beta1.Sandbox{
				EnvFrom: []v1beta1.EnvFromSource{{Credential: &v1beta1.CredentialRef{Name: "vault"}, Prefix: "VAULT_"}},
			},
		},
		"HTTPNoHost":      {reason: "An HTTP rule needs a host or a pattern.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Methods: []string{"GET"}}}}}, want: "sandbox.egress.http[0] must set exactly one of host and hostPattern"},
		"HTTPBothHosts":   {reason: "Not both.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", HostPattern: "*.a", Methods: []string{"GET"}}}}}, want: "sandbox.egress.http[0] must set exactly one of host and hostPattern"},
		"HTTPNoMethods":   {reason: "Nothing is admitted implicitly.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a"}}}}, want: "sandbox.egress.http[0].methods must list at least one method"},
		"HTTPHostPort":    {reason: "A host is a bare name: a port (or scheme, zone, whitespace-only) is refused at the shape, so a Composition never ships a rule that can never match.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com:8443", Methods: []string{"GET"}}}}}, want: `sandbox.egress.http[0].host "api.example.com:8443" must be a bare host name`},
		"HTTPHostDot":     {reason: "A host that normalizes to nothing is refused before it can become a wildcard.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: ".", Methods: []string{"GET"}}}}}, want: `sandbox.egress.http[0].host "." must be a bare host name`},
		"HTTPPrefixDots":  {reason: "A path prefix is normalized, like the paths it is compared with.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET"}, PathPrefix: "/v1/../x"}}}}, want: `sandbox.egress.http[0].pathPrefix "/v1/../x" must be normalized`},
		"HTTPBadPattern":  {reason: "A host pattern has exactly one leading wildcard label.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{HostPattern: "api.*.example.com", Methods: []string{"GET"}}}}}, want: `sandbox.egress.http[0].hostPattern "api.*.example.com" must be a host name with one leading wildcard label`},
		"HTTPBadMethod":   {reason: "Methods come from the CRD's enum.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET", "FETCH"}}}}}, want: `sandbox.egress.http[0].methods: "FETCH" is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS`},
		"HTTPPathPrefix":  {reason: "A path prefix is a path.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET"}, PathPrefix: "v1"}}}}, want: `sandbox.egress.http[0].pathPrefix "v1" must start with /`},
		"HTTPSecondEntry": {reason: "The index names the offending entry.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET"}}, {Methods: []string{"GET"}}}}}, want: "sandbox.egress.http[1] must set exactly one of host and hostPattern"},
		"EnvBadName": {
			reason:  "Environment names are identifiers.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "log-level", Value: new("debug")}}},
			want:    `sandbox.env[0].name "log-level" is not an identifier`,
		},
		"EnvBadNameDigit": {
			reason:  "An identifier does not start with a digit.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "1X", Value: new("y")}}},
			want:    `sandbox.env[0].name "1X" is not an identifier`,
		},
		"EnvNeitherValueNorFrom": {
			reason:  "Exactly one of value and valueFrom must be set.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X"}}},
			want:    "sandbox.env[0]: exactly one of value and valueFrom must be set",
		},
		"EnvBothValueAndFrom": {
			reason:  "Not both.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X", Value: new("v"), ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "c", Key: "k"}}}}},
			want:    "sandbox.env[0]: exactly one of value and valueFrom must be set",
		},
		"EnvValueNUL": {
			reason:  "WASI passes the environment as NUL-terminated strings; a NUL in a literal value would truncate it.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X", Value: new("a\x00b")}}},
			want:    "sandbox.env[0]: the value of X must not contain a NUL byte",
		},
		"EnvValueFromNoSource": {
			reason:  "A valueFrom without a source member is refused.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X", ValueFrom: &v1beta1.ValueSource{}}}},
			want:    "sandbox.env[0].valueFrom: exactly one source must be set (credential)",
		},
		"EnvValueFromEmptyName": {
			reason:  "A credential reference needs a name.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Key: "k"}}}}},
			want:    "sandbox.env[0].valueFrom.credential.name must not be empty",
		},
		"EnvValueFromEmptyKey": {
			reason:  "A credential reference needs a key.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "c"}}}}},
			want:    "sandbox.env[0].valueFrom.credential.key must not be empty",
		},
		"EnvDuplicate": {
			reason:  "A name set twice is refused.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "X", Value: new("a")}, {Name: "X", Value: new("b")}}},
			want:    "sandbox.env[1]: X is already set by sandbox.env[0]",
		},
		"EnvFromNoSource": {
			reason:  "An envFrom without a source member is refused.",
			sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{{}}},
			want:    "sandbox.envFrom[0]: exactly one source must be set (credential)",
		},
		"EnvFromEmptyName": {
			reason:  "An envFrom credential needs a name.",
			sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{{Credential: &v1beta1.CredentialRef{}}}},
			want:    "sandbox.envFrom[0].credential.name must not be empty",
		},
		"EnvFromBadPrefix": {
			reason:  "A prefix must be a valid identifier prefix.",
			sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{{Credential: &v1beta1.CredentialRef{Name: "c"}, Prefix: "1BAD"}}},
			want:    `sandbox.envFrom[0].prefix "1BAD" is not a valid identifier prefix`,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := Validate(tc.sandbox)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nValidate(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nValidate(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

// TestValidateRules pins that the rule checks are shared: the same words,
// under whatever field name the caller gives (a Composition's
// sandbox.egress.http, a manifest's requires.egress.http).
func TestValidateRules(t *testing.T) {
	cases := map[string]struct {
		reason string
		field  string
		rules  []v1beta1.SandboxHTTPRule
		want   string
	}{
		"OK":       {reason: "Well-formed rules pass under any field name.", field: "requires.egress.http", rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}, PathPrefix: "/v1/"}}},
		"Manifest": {reason: "A manifest's rule is named as the manifest's.", field: "requires.egress.http", rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com"}}, want: "requires.egress.http[0].methods must list at least one method"},
		"Index":    {reason: "The index names the rule.", field: "sandbox.egress.http", rules: []v1beta1.SandboxHTTPRule{{Host: "a.example.com", Methods: []string{"GET"}}, {HostPattern: "nope", Methods: []string{"GET"}}}, want: `sandbox.egress.http[1].hostPattern "nope" must be a host name with one leading wildcard label, e.g. *.example.com`},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := ValidateRules(tc.field, tc.rules)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nValidateRules(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || err.Error() != tc.want {
				t.Fatalf("\n%s\nValidateRules(): want %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

func TestValidEnvKey(t *testing.T) {
	for key, want := range map[string]bool{"A": true, "_x1": true, "GREETING_STYLE": true, "1x": false, "a-b": false, "": false, "a b": false} {
		if got := ValidEnvKey(key); got != want {
			t.Errorf("ValidEnvKey(%q) = %v, want %v", key, got, want)
		}
	}
}
