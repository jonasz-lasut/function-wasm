package module

import (
	"context"
	"crypto"
	"crypto/ecdh"
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

// pushSignature publishes a cosign-style signature artifact for ref whose
// payload claims the manifest digest of ref, or claims instead when set.
func pushSignature(t *testing.T, ref name.Digest, key crypto.Signer, claims string) {
	t.Helper()
	repo, manifestDigest := ref.Context(), ref.DigestStr()
	signedDigest := manifestDigest
	if claims != "" {
		signedDigest = claims
	}
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

// simplePayload is a cosign simple-signing payload naming digest, with the
// given critical.type (cosignPayloadType for a well-formed one).
func simplePayload(digest, typ string) []byte {
	return []byte(`{"critical":{"identity":{"docker-reference":"x"},"image":{"docker-manifest-digest":"` + digest + `"},"type":"` + typ + `"},"optional":null}`)
}

// pushRawSignature publishes a signature artifact for ref with one layer
// carrying payload verbatim. ann is the raw signature annotation value (not
// re-encoded, so a caller can push invalid base64); withAnn omits the
// annotation entirely to model a layer that is not a signature.
func pushRawSignature(t *testing.T, ref name.Digest, payload []byte, ann string, withAnn bool) {
	t.Helper()
	repo, manifestDigest := ref.Context(), ref.DigestStr()
	annotations := map[string]string{}
	if withAnn {
		annotations[cosignSignatureAnnotation] = ann
	}
	img, err := mutate.Append(empty.Image, mutate.Addendum{
		Layer:       static.NewLayer(payload, "application/vnd.dev.cosign.simplesigning.v1+json"),
		Annotations: annotations,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := remote.Write(repo.Tag(SignatureTag(manifestDigest)), img); err != nil {
		t.Fatalf("cannot push signature: %v", err)
	}
}

func digestRef(t *testing.T, ref string) name.Digest {
	t.Helper()
	d, err := name.NewDigest(ref)
	if err != nil {
		t.Fatal(err)
	}
	return d
}

func oci(ref string) v1beta1.ModuleSource {
	return v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: ref}}
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
	signedRef := artifact(t, host, "signed", layer)
	pushSignature(t, digestRef(t, signedRef), ecKey, "")
	rsaRef := artifact(t, host, "rsa", layer)
	pushSignature(t, digestRef(t, rsaRef), rsaKey, "")
	edRef := artifact(t, host, "ed", layer)
	pushSignature(t, digestRef(t, edRef), edKey, "")
	unsignedRef := artifact(t, host, "unsigned", layer)
	wrongPayloadRef := artifact(t, host, "wrongpayload", layer)
	pushSignature(t, digestRef(t, wrongPayloadRef), ecKey, otherDigest)

	validB64 := base64.StdEncoding.EncodeToString([]byte("x"))
	notB64Ref := artifact(t, host, "notb64", layer)
	pushRawSignature(t, digestRef(t, notB64Ref), simplePayload(digestRef(t, notB64Ref).DigestStr(), cosignPayloadType), "not base64!!", true)
	notJSONRef := artifact(t, host, "notjson", layer)
	pushRawSignature(t, digestRef(t, notJSONRef), []byte("not json at all"), validB64, true)
	wrongTypeRef := artifact(t, host, "wrongtype", layer)
	pushRawSignature(t, digestRef(t, wrongTypeRef), simplePayload(digestRef(t, wrongTypeRef).DigestStr(), "not a cosign signature"), validB64, true)
	noLayerRef := artifact(t, host, "nolayer", layer)
	pushRawSignature(t, digestRef(t, noLayerRef), simplePayload(digestRef(t, noLayerRef).DigestStr(), cosignPayloadType), "", false)

	cases := map[string]struct {
		reason string
		keys   [][]byte
		src    v1beta1.ModuleSource
		want   string
	}{
		"ECDSA": {
			reason: "A module signed with the configured ECDSA key resolves.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(signedRef),
		},
		"RSA": {
			reason: "RSA PKCS#1 v1.5 signatures verify too.",
			keys:   [][]byte{pemPublic(t, &rsaKey.PublicKey)},
			src:    oci(rsaRef),
		},
		"Ed25519": {
			reason: "ed25519 signatures verify too.",
			keys:   [][]byte{pemPublic(t, edKey.Public())},
			src:    oci(edRef),
		},
		"SecondKeyMatches": {
			reason: "Any of several configured keys is enough.",
			keys:   [][]byte{pemPublic(t, &otherKey.PublicKey), pemPublic(t, &ecKey.PublicKey)},
			src:    oci(signedRef),
		},
		"Unsigned": {
			reason: "A module without a signature artifact is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(unsignedRef),
			want:   "carries no cosign signature",
		},
		"WrongKey": {
			reason: "A signature by another key is refused.",
			keys:   [][]byte{pemPublic(t, &otherKey.PublicKey)},
			src:    oci(signedRef),
			want:   "signature does not verify with the configured keys",
		},
		"PayloadForOtherDigest": {
			reason: "A valid signature over a payload naming another manifest is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(wrongPayloadRef),
			want:   "payload signs " + otherDigest,
		},
		"SignatureNotBase64": {
			reason: "A signature annotation that is not base64 is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(notB64Ref),
			want:   "signature is not base64",
		},
		"PayloadNotJSON": {
			reason: "A payload that is not simple-signing JSON is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(notJSONRef),
			want:   "payload is not simple-signing JSON",
		},
		"PayloadWrongType": {
			reason: "A payload whose critical.type is not a cosign signature is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(wrongTypeRef),
			want:   `payload type is "not a cosign signature"`,
		},
		"NoSignatureLayer": {
			reason: "A signature artifact with no signature-annotated layer is refused.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    oci(noLayerRef),
			want:   "has no cosign signature layers",
		},
		"PathRefused": {
			reason: "With a verifier configured only OCI sources are accepted.",
			keys:   [][]byte{pemPublic(t, &ecKey.PublicKey)},
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"},
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
			if err != nil {
				// Only the source shape is judged at resolve time.
				if tc.want != "" && strings.Contains(err.Error(), tc.want) {
					return
				}
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			err = ref.Verify(context.Background())
			if tc.want != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want) {
					t.Fatalf("\n%s\nVerify(): want error containing %q, got %v", tc.reason, tc.want, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nVerify(): %v", tc.reason, err)
			}
			// The second check is served from the verified set.
			if err := ref.Verify(context.Background()); err != nil {
				t.Fatalf("\n%s\nsecond Verify(): %v", tc.reason, err)
			}
			if _, err := ref.Fetch(context.Background()); err != nil {
				t.Fatalf("\n%s\nFetch(): %v", tc.reason, err)
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

	t.Run("UnsupportedKeyType", func(t *testing.T) {
		// An X25519 key parses as a valid PKIX public key but is not one cosign
		// signs with, so it is refused rather than silently ignored.
		k, err := ecdh.X25519().GenerateKey(rand.Reader)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := NewVerifier(pemPublic(t, k.Public())); err == nil || !strings.Contains(err.Error(), "unsupported cosign public key type") {
			t.Fatalf("NewVerifier() of an X25519 key: want unsupported-type error, got %v", err)
		}
	})
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
