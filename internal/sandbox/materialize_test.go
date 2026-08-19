package sandbox

import (
	"strings"
	"testing"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/google/go-cmp/cmp"
)

func TestMaterialize(t *testing.T) {
	creds := map[string]*fnv1.Credentials{
		"aws": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
			Data: map[string][]byte{
				"access_key_id":     []byte("AKIAIOSFODNN7EXAMPLE"),
				"secret_access_key": []byte("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
			},
		}}},
		"vault": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
			Data: map[string][]byte{
				"TOKEN": []byte("s.abcdef123456"),
				"ADDR":  []byte("https://vault.internal:8200"),
			},
		}}},
		"pull": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
			Data: map[string][]byte{".dockerconfigjson": []byte(`{"auths":{}}`)},
		}}},
	}

	type args struct {
		bindings []EnvBinding
		sources  Sources
	}
	cases := map[string]struct {
		reason string
		args   args
		want   map[string]string
		err    string
	}{
		"None": {
			reason: "No bindings materialize to nil.",
			args:   args{sources: Sources{Credentials: creds}},
		},
		"OneBinding": {
			reason: "A binding reads its key from the step credential.",
			args: args{
				bindings: []EnvBinding{{Name: "AWS_ACCESS_KEY_ID", FromCredential: CredentialKey{Name: "aws", Key: "access_key_id"}}},
				sources:  Sources{Credentials: creds},
			},
			want: map[string]string{"AWS_ACCESS_KEY_ID": "AKIAIOSFODNN7EXAMPLE"},
		},
		"SeveralBindings": {
			reason: "Bindings combine across credentials.",
			args: args{
				bindings: []EnvBinding{
					{Name: "AWS_ACCESS_KEY_ID", FromCredential: CredentialKey{Name: "aws", Key: "access_key_id"}},
					{Name: "VAULT_TOKEN", FromCredential: CredentialKey{Name: "vault", Key: "TOKEN"}},
				},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"AWS_ACCESS_KEY_ID": "AKIAIOSFODNN7EXAMPLE", "VAULT_TOKEN": "s.abcdef123456"},
		},
		"PullCredentialRefused": {
			reason: "The pull credential is never a source.",
			args: args{
				bindings: []EnvBinding{{Name: "X", FromCredential: CredentialKey{Name: "pull", Key: ".dockerconfigjson"}}},
				sources:  Sources{Credentials: creds, Withheld: "pull"},
			},
			err: `credential "pull" is the pull credential and cannot be used as a source`,
		},
		"MissingCredential": {
			reason: "A missing credential is refused, telling the author where to declare it.",
			args: args{
				bindings: []EnvBinding{{Name: "X", FromCredential: CredentialKey{Name: "gone", Key: "k"}}},
				sources:  Sources{Credentials: creds},
			},
			err: `requires.env[0] (X): the request carries no credential "gone"; declare it on the pipeline step`,
		},
		"MissingKey": {
			reason: "A missing key is refused.",
			args: args{
				bindings: []EnvBinding{{Name: "X", FromCredential: CredentialKey{Name: "aws", Key: "nope"}}},
				sources:  Sources{Credentials: creds},
			},
			err: `credential "aws" has no key "nope"`,
		},
		"NULInResolvedValue": {
			reason: "A NUL byte in a resolved credential value is refused.",
			args: args{
				bindings: []EnvBinding{{Name: "X", FromCredential: CredentialKey{Name: "bad", Key: "k"}}},
				sources: Sources{Credentials: map[string]*fnv1.Credentials{
					"bad": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
						Data: map[string][]byte{"k": []byte("a\x00b")},
					}}},
				}},
			},
			err: "the value of X contains a NUL byte",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := Materialize(tc.args.bindings, tc.args.sources)
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nMaterialize(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nMaterialize(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("\n%s\nMaterialize(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}
