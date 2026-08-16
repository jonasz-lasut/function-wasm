package module

import (
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/ed25519"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-containerregistry/pkg/name"
	"github.com/google/go-containerregistry/pkg/registry"
	"github.com/google/go-containerregistry/pkg/v1/empty"
	"github.com/google/go-containerregistry/pkg/v1/mutate"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/static"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

func pemPublic(t *testing.T, pub crypto.PublicKey) []byte {
	t.Helper()
	der, err := x509.MarshalPKIXPublicKey(pub)
	if err != nil {
		t.Fatal(err)
	}
	return pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: der})
}

// sign produces a cosign-compatible signature over payload for the key type.
func sign(t *testing.T, key crypto.Signer, payload []byte) []byte {
	t.Helper()
	sum := sha256.Sum256(payload)
	var sig []byte
	var err error
	switch k := key.(type) {
	case *ecdsa.PrivateKey:
		sig, err = ecdsa.SignASN1(rand.Reader, k, sum[:])
	case *rsa.PrivateKey:
		sig, err = rsa.SignPKCS1v15(rand.Reader, k, crypto.SHA256, sum[:])
	case ed25519.PrivateKey:
		sig = ed25519.Sign(k, payload)
	default:
		t.Fatalf("unsupported key %T", key)
	}
	if err != nil {
		t.Fatal(err)
	}
	return sig
}

// pushSignature publishes a cosign-style signature artifact for
// manifestDigest whose payload claims signedDigest.
func pushSignature(t *testing.T, repo name.Repository, manifestDigest, signedDigest string, key crypto.Signer) {
	t.Helper()
	payload := []byte(`{"critical":{"identity":{"docker-reference":"` + repo.String() + `"},"image":{"docker-manifest-digest":"` + signedDigest + `"},"type":"cosign container image signature"},"optional":null}`)
	img, err := mutate.Append(empty.Image, mutate.Addendum{
		Layer:       static.NewLayer(payload, "application/vnd.dev.cosign.simplesigning.v1+json"),
		Annotations: map[string]string{cosignSignatureAnnotation: base64.StdEncoding.EncodeToString(sign(t, key, payload))},
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := remote.Write(repo.Tag(SignatureTag(manifestDigest)), img); err != nil {
		t.Fatalf("cannot push signature: %v", err)
	}
}

func TestVerifier(t *testing.T) {
	handler := registry.New()
	srv := httptest.NewServer(handler)
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://")

	ecKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	otherKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	rsaKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	_, edKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}

	layer := static.NewLayer(module, "application/wasm")
	signedRepo, signedDigest := artifact(t, host, "signed", layer)
	pushSignature(t, signedRepo, signedDigest, signedDigest, ecKey)
	rsaRepo, rsaDigest := artifact(t, host, "rsa", layer)
	pushSignature(t, rsaRepo, rsaDigest, rsaDigest, rsaKey)
	edRepo, edDigest := artifact(t, host, "ed", layer)
	pushSignature(t, edRepo, edDigest, edDigest, edKey)
	unsignedRepo, _ := artifact(t, host, "unsigned", layer)
	wrongPayloadRepo, wrongPayloadDigest := artifact(t, host, "wrongpayload", layer)
	pushSignature(t, wrongPayloadRepo, wrongPayloadDigest, otherDigest, ecKey)

	cases := map[string]struct {
		reason string
		keys   [][]byte
		src    v1beta1.ModuleSource
		want   string
	}{
		"ECDSA": {
			reason: "A module signed with the configured ECDSA key resolves.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: signedRepo.String() + ":v1"}},
		},
		"RSA": {
			reason: "RSA PKCS#1 v1.5 signatures verify too.",
			keys:   [][]byte{pemPublic(t, &rsaKey.PublicKey)},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: rsaRepo.String() + ":v1"}},
		},
		"Ed25519": {
			reason: "ed25519 signatures verify too.",
			keys:   [][]byte{pemPublic(t, edKey.Public())},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: edRepo.String() + ":v1"}},
		},
		"SecondKeyMatches": {
			reason: "Any of several configured keys is enough.",
			keys:   [][]byte{pemPublic(t, &otherKey.PublicKey), pemPublic(t, &ecKey.PublicKey)},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: signedRepo.String() + ":v1"}},
		},
		"Unsigned": {
			reason: "A module without a signature artifact is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: unsignedRepo.String() + ":v1"}},
			want:   "carries no cosign signature",
		},
		"WrongKey": {
			reason: "A signature by another key is refused.",
			keys:   [][]byte{pemPublic(t, &otherKey.PublicKey)},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: signedRepo.String() + ":v1"}},
			want:   "signature does not verify with the configured keys",
		},
		"PayloadForOtherDigest": {
			reason: "A valid signature over a payload naming another manifest is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: wrongPayloadRepo.String() + ":v1"}},
			want:   "payload signs " + otherDigest,
		},
		"PathRefused": {
			reason: "With a verifier configured only OCI sources are accepted.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    v1beta1.ModuleSource{Path: "fn.wasm"},
			want:   "only cosign-signed oci modules are accepted",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var pemKeys []byte
			for _, k := range tc.keys {
				pemKeys = append(pemKeys, k...)
			}
			verifier, err := NewVerifier(pemKeys)
			if err != nil {
				t.Fatalf("NewVerifier(): %v", err)
			}
			r, err := NewResolver(Options{Verifier: verifier})
			if err != nil {
				t.Fatal(err)
			}
			ref, err := r.Resolve(context.Background(), tc.src, nil)
			if tc.want != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want) {
					t.Fatalf("\n%s\nResolve(): want error containing %q, got %v", tc.reason, tc.want, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			if _, err := ref.Fetch(context.Background()); err != nil {
				t.Fatalf("\n%s\nFetch(): %v", tc.reason, err)
			}
			// The second resolution is served from the verified set.
			if _, err := r.Resolve(context.Background(), tc.src, nil); err != nil {
				t.Fatalf("\n%s\nsecond Resolve(): %v", tc.reason, err)
			}
		})
	}
}

func TestNewVerifierErrors(t *testing.T) {
	cases := map[string]struct {
		reason string
		pem    string
		want   string
	}{
		"Empty":   {reason: "No PEM block is an error.", pem: "", want: "no PEM public key found"},
		"Garbage": {reason: "A PEM block that is not a public key is an error.", pem: "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n", want: "cannot parse cosign public key"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := NewVerifier([]byte(tc.pem))
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nNewVerifier(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

func TestLoadVerifier(t *testing.T) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "cosign.pub")
	if err := os.WriteFile(path, pemPublic(t, &key.PublicKey), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadVerifier(path); err != nil {
		t.Errorf("LoadVerifier(): %v", err)
	}
	if _, err := LoadVerifier(filepath.Join(t.TempDir(), "missing.pub")); err == nil {
		t.Error("LoadVerifier() of a missing file succeeded")
	}
}
