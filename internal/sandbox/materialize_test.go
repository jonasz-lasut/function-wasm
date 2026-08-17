package sandbox

import (
	"strings"
	"testing"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/google/go-cmp/cmp"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
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
		sandbox *v1beta1.Sandbox
		sources Sources
	}
	cases := map[string]struct {
		reason string
		args   args
		want   map[string]string
		err    string
	}{
		"Nil": {
			reason: "A nil sandbox materializes to nil.",
			args:   args{sandbox: nil},
		},
		"Empty": {
			reason: "No env or envFrom materializes to nil.",
			args:   args{sandbox: &v1beta1.Sandbox{}},
		},
		"LiteralOnly": {
			reason: "Literal values are copied as-is.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "AWS_REGION", Value: new("eu-central-1")},
					{Name: "LOG_LEVEL", Value: new("debug")},
				}},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"AWS_REGION": "eu-central-1", "LOG_LEVEL": "debug"},
		},
		"ValueFrom": {
			reason: "A valueFrom reads from the step credential.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "AWS_ACCESS_KEY_ID", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "aws", Key: "access_key_id"}}},
				}},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"AWS_ACCESS_KEY_ID": "AKIAIOSFODNN7EXAMPLE"},
		},
		"MixedLiteralAndFrom": {
			reason: "Literals and valueFrom entries coexist.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "AWS_REGION", Value: new("us-east-1")},
					{Name: "AWS_ACCESS_KEY_ID", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "aws", Key: "access_key_id"}}},
				}},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"AWS_REGION": "us-east-1", "AWS_ACCESS_KEY_ID": "AKIAIOSFODNN7EXAMPLE"},
		},
		"EnvFrom": {
			reason: "EnvFrom imports every key of the credential.",
			args: args{
				sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{
					{Credential: &v1beta1.CredentialRef{Name: "vault"}},
				}},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"TOKEN": "s.abcdef123456", "ADDR": "https://vault.internal:8200"},
		},
		"EnvFromWithPrefix": {
			reason: "EnvFrom with a prefix prepends it.",
			args: args{
				sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{
					{Credential: &v1beta1.CredentialRef{Name: "vault"}, Prefix: "VAULT_"},
				}},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"VAULT_TOKEN": "s.abcdef123456", "VAULT_ADDR": "https://vault.internal:8200"},
		},
		"EnvAndEnvFrom": {
			reason: "env[] and envFrom[] combine.",
			args: args{
				sandbox: &v1beta1.Sandbox{
					Env: []v1beta1.EnvVar{
						{Name: "AWS_REGION", Value: new("us-east-1")},
					},
					EnvFrom: []v1beta1.EnvFromSource{
						{Credential: &v1beta1.CredentialRef{Name: "vault"}, Prefix: "VAULT_"},
					},
				},
				sources: Sources{Credentials: creds},
			},
			want: map[string]string{"AWS_REGION": "us-east-1", "VAULT_TOKEN": "s.abcdef123456", "VAULT_ADDR": "https://vault.internal:8200"},
		},
		"PullCredentialRefused": {
			reason: "The pull credential is never a source.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "X", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "pull", Key: ".dockerconfigjson"}}},
				}},
				sources: Sources{Credentials: creds, Withheld: "pull"},
			},
			err: `credential "pull" is the pull credential and cannot be used as a source`,
		},
		"PullCredentialRefusedEnvFrom": {
			reason: "The pull credential is refused for envFrom too.",
			args: args{
				sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{
					{Credential: &v1beta1.CredentialRef{Name: "pull"}},
				}},
				sources: Sources{Credentials: creds, Withheld: "pull"},
			},
			err: `credential "pull" is the pull credential and cannot be used as a source`,
		},
		"MissingCredential": {
			reason: "A missing credential is refused.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "X", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "gone", Key: "k"}}},
				}},
				sources: Sources{Credentials: creds},
			},
			err: `the request carries no credential "gone"`,
		},
		"MissingKey": {
			reason: "A missing key is refused.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "X", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "aws", Key: "nope"}}},
				}},
				sources: Sources{Credentials: creds},
			},
			err: `credential "aws" has no key "nope"`,
		},
		"NULInResolvedValue": {
			reason: "A NUL byte in a resolved credential value is refused.",
			args: args{
				sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{
					{Name: "X", ValueFrom: &v1beta1.ValueSource{Credential: &v1beta1.CredentialKeyRef{Name: "bad", Key: "k"}}},
				}},
				sources: Sources{Credentials: map[string]*fnv1.Credentials{
					"bad": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
						Data: map[string][]byte{"k": []byte("a\x00b")},
					}}},
				}},
			},
			err: "the value of X contains a NUL byte",
		},
		"EnvFromBadKey": {
			reason: "An envFrom key that is not a valid variable name refuses the run.",
			args: args{
				sandbox: &v1beta1.Sandbox{EnvFrom: []v1beta1.EnvFromSource{
					{Credential: &v1beta1.CredentialRef{Name: "pull"}},
				}},
				sources: Sources{Credentials: map[string]*fnv1.Credentials{
					"pull": {Source: &fnv1.Credentials_CredentialData{CredentialData: &fnv1.CredentialData{
						Data: map[string][]byte{".dockerconfigjson": []byte(`{}`)},
					}}},
				}},
			},
			err: `credential "pull" has key ".dockerconfigjson", which is not an environment variable name`,
		},
		"EnvFromDuplicateWithEnv": {
			reason: "An envFrom key that collides with an env[] name is refused.",
			args: args{
				sandbox: &v1beta1.Sandbox{
					Env: []v1beta1.EnvVar{
						{Name: "TOKEN", Value: new("override")},
					},
					EnvFrom: []v1beta1.EnvFromSource{
						{Credential: &v1beta1.CredentialRef{Name: "vault"}},
					},
				},
				sources: Sources{Credentials: creds},
			},
			err: "TOKEN is already set by sandbox.env[0]",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := Materialize(tc.args.sandbox, tc.args.sources)
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
