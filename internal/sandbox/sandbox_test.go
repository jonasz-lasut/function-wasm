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
				Env:        map[string]string{"LOG_LEVEL": "debug", "_x1": "y"},
			},
		},
		"HTTPNoHost":      {reason: "An HTTP rule needs a host or a pattern.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Methods: []string{"GET"}}}}}, want: "sandbox.egress.http[0] must set exactly one of host and hostPattern"},
		"HTTPBothHosts":   {reason: "Not both.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", HostPattern: "*.a", Methods: []string{"GET"}}}}}, want: "sandbox.egress.http[0] must set exactly one of host and hostPattern"},
		"HTTPNoMethods":   {reason: "Nothing is admitted implicitly.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a"}}}}, want: "sandbox.egress.http[0].methods must list at least one method"},
		"HTTPHostPort":    {reason: "A host is a bare name: a port (or scheme, zone, whitespace-only) is refused at the shape, so a Composition never ships a rule that can never match.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com:8443", Methods: []string{"GET"}}}}}, want: `sandbox.egress.http[0].host "api.example.com:8443" must be a bare host name`},
		"HTTPHostDot":     {reason: "A host that normalizes to nothing is refused before it can become a wildcard.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: ".", Methods: []string{"GET"}}}}}, want: `sandbox.egress.http[0].host "." must be a bare host name`},
		"HTTPPrefixDots":  {reason: "A path prefix is normalized, like the paths it is compared with.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET"}, PathPrefix: "/v1/../x"}}}}, want: `sandbox.egress.http[0].pathPrefix "/v1/../x" must be normalized`},
		"HTTPBadPattern":  {reason: "A host pattern has exactly one leading wildcard label — the CRD says so, and so must the runtime, since Crossplane never installs the CRD.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{HostPattern: "api.*.example.com", Methods: []string{"GET"}}}}}, want: `sandbox.egress.http[0].hostPattern "api.*.example.com" must be a host name with one leading wildcard label`},
		"HTTPBadMethod":   {reason: "Methods come from the CRD's enum.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET", "FETCH"}}}}}, want: `sandbox.egress.http[0].methods: "FETCH" is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS`},
		"HTTPPathPrefix":  {reason: "A path prefix is a path.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET"}, PathPrefix: "v1"}}}}, want: `sandbox.egress.http[0].pathPrefix "v1" must start with /`},
		"HTTPSecondEntry": {reason: "The index names the offending entry.", sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "a", Methods: []string{"GET"}}, {Methods: []string{"GET"}}}}}, want: "sandbox.egress.http[1] must set exactly one of host and hostPattern"},
		"EnvKey":          {reason: "Environment keys are identifiers.", sandbox: &v1beta1.Sandbox{Env: map[string]string{"log-level": "debug"}}, want: `sandbox.env key "log-level" is not an identifier`},
		"EnvKeyDigit":     {reason: "An identifier does not start with a digit.", sandbox: &v1beta1.Sandbox{Env: map[string]string{"1X": "y"}}, want: `sandbox.env key "1X" is not an identifier`},
		"EnvValueNUL":     {reason: "WASI passes the environment as NUL-terminated strings; a NUL in a value would truncate it.", sandbox: &v1beta1.Sandbox{Env: map[string]string{"X": "a\x00b"}}, want: "sandbox.env X: the value must not contain a NUL byte"},
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
