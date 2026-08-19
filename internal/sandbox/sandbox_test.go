package sandbox

import (
	"testing"
)

func TestValidateBindings(t *testing.T) {
	cases := map[string]struct {
		reason   string
		field    string
		bindings []EnvBinding
		want     string
	}{
		"OK": {
			reason: "Well-formed bindings pass.",
			field:  "requires.env",
			bindings: []EnvBinding{
				{Name: "DATABASE_URL", FromCredential: CredentialKey{Name: "db", Key: "url"}},
				{Name: "TOKEN", FromCredential: CredentialKey{Name: "api", Key: "token"}},
			},
		},
		"BadName": {
			reason:   "The variable name must be an identifier.",
			field:    "requires.env",
			bindings: []EnvBinding{{Name: "1x", FromCredential: CredentialKey{Name: "db", Key: "url"}}},
			want:     `requires.env[0].name "1x" is not an identifier ([A-Za-z_][A-Za-z0-9_]*)`,
		},
		"NoCredential": {
			reason:   "A binding names its credential.",
			field:    "requires.env",
			bindings: []EnvBinding{{Name: "TOKEN"}},
			want:     "requires.env[0].fromCredential.name must not be empty",
		},
		"NoKey": {
			reason:   "And exactly one key of it.",
			field:    "requires.env",
			bindings: []EnvBinding{{Name: "TOKEN", FromCredential: CredentialKey{Name: "api"}}},
			want:     "requires.env[0].fromCredential.key must not be empty",
		},
		"Duplicate": {
			reason: "A variable bound twice is refused, naming both entries.",
			field:  "requires.env",
			bindings: []EnvBinding{
				{Name: "TOKEN", FromCredential: CredentialKey{Name: "api", Key: "a"}},
				{Name: "TOKEN", FromCredential: CredentialKey{Name: "api", Key: "b"}},
			},
			want: "requires.env[1]: TOKEN is already bound by requires.env[0]",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := ValidateBindings(tc.field, tc.bindings)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nValidateBindings(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || err.Error() != tc.want {
				t.Fatalf("\n%s\nValidateBindings(): want %q, got %v", tc.reason, tc.want, err)
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
