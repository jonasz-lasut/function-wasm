package module

import (
	"bytes"
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/ed25519"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"

	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/remote/transport"
)

// cosign stores an image's signatures as an artifact tagged after the
// manifest digest; each layer is a simple-signing payload with the base64
// signature in an annotation.
const (
	cosignSignatureAnnotation = "dev.cosignproject.cosign/signature"
	cosignPayloadType         = "cosign container image signature"

	// maxSignaturePayload bounds a signature payload read from a registry.
	maxSignaturePayload = 1 << 20
)

// Verifier checks cosign signatures made with a fixed set of public keys —
// the "cosign sign --key" workflow. Keyless (Fulcio/Rekor) signatures are not
// supported.
type Verifier struct {
	keys []crypto.PublicKey

	mu       sync.Mutex
	verified map[string]struct{} // manifest digests already verified
}

// LoadVerifier reads one or more PEM public keys (as cosign.pub is written)
// from path.
func LoadVerifier(path string) (*Verifier, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // The operator's own --cosign-key flag.
	if err != nil {
		return nil, fmt.Errorf("cannot read cosign public key: %w", err)
	}
	return NewVerifier(raw)
}

// NewVerifier parses PEM public keys.
func NewVerifier(pemKeys []byte) (*Verifier, error) {
	v := &Verifier{verified: map[string]struct{}{}}
	rest := pemKeys
	for {
		var block *pem.Block
		block, rest = pem.Decode(rest)
		if block == nil {
			break
		}
		key, err := x509.ParsePKIXPublicKey(block.Bytes)
		if err != nil {
			return nil, fmt.Errorf("cannot parse cosign public key: %w", err)
		}
		switch key.(type) {
		case *ecdsa.PublicKey, *rsa.PublicKey, ed25519.PublicKey:
			v.keys = append(v.keys, key)
		default:
			return nil, fmt.Errorf("unsupported cosign public key type %T", key)
		}
	}
	if len(v.keys) == 0 {
		return nil, errors.New("no PEM public key found for cosign verification")
	}
	return v, nil
}

// SignatureTag is the tag cosign uses for the signatures of a manifest.
func SignatureTag(manifestDigest string) string {
	return strings.Replace(manifestDigest, ":", "-", 1) + ".sig"
}

// Verify checks that the manifest ref points to carries a signature by one of
// the keys. Results are remembered per manifest digest for the life of the
// process; manifests are immutable by digest.
func (v *Verifier) Verify(ctx context.Context, ref name.Digest, opts []remote.Option) error {
	digest := ref.DigestStr()
	v.mu.Lock()
	_, done := v.verified[digest]
	v.mu.Unlock()
	if done {
		return nil
	}

	sigRef := ref.Context().Tag(SignatureTag(digest))
	desc, err := remote.Get(sigRef, append(opts, remote.WithContext(ctx))...)
	if err != nil {
		var terr *transport.Error
		if errors.As(err, &terr) && terr.StatusCode == 404 {
			return fmt.Errorf("%s carries no cosign signature (%s not found)", ref, sigRef)
		}
		return fmt.Errorf("cannot fetch cosign signature %s: %w", sigRef, err)
	}
	m, err := v1.ParseManifest(bytes.NewReader(desc.Manifest))
	if err != nil {
		return fmt.Errorf("cannot parse cosign signature %s: %w", sigRef, err)
	}

	var reasons []string
	for _, layer := range m.Layers {
		encoded, ok := layer.Annotations[cosignSignatureAnnotation]
		if !ok {
			continue
		}
		sig, err := base64.StdEncoding.DecodeString(encoded)
		if err != nil {
			reasons = append(reasons, "signature is not base64")
			continue
		}
		l, err := remote.Layer(ref.Context().Digest(layer.Digest.String()), append(opts, remote.WithContext(ctx))...)
		if err != nil {
			return fmt.Errorf("cannot fetch cosign payload: %w", err)
		}
		rc, err := l.Compressed()
		if err != nil {
			return fmt.Errorf("cannot read cosign payload: %w", err)
		}
		payload, err := readCapped(rc, maxSignaturePayload)
		_ = rc.Close()
		if err != nil {
			return fmt.Errorf("cannot read cosign payload: %w", err)
		}
		if err := checkPayload(payload, digest); err != nil {
			reasons = append(reasons, err.Error())
			continue
		}
		if v.verifies(payload, sig) {
			v.mu.Lock()
			v.verified[digest] = struct{}{}
			v.mu.Unlock()
			return nil
		}
		reasons = append(reasons, "signature does not verify with the configured keys")
	}
	if len(reasons) == 0 {
		return fmt.Errorf("%s has no cosign signature layers", sigRef)
	}
	return fmt.Errorf("no valid cosign signature for %s: %s", ref, strings.Join(reasons, "; "))
}

// simpleSigning is the part of cosign's payload that binds a signature to a
// manifest digest.
type simpleSigning struct {
	Critical struct {
		Image struct {
			DockerManifestDigest string `json:"docker-manifest-digest"`
		} `json:"image"`
		Type string `json:"type"`
	} `json:"critical"`
}

func checkPayload(payload []byte, digest string) error {
	ss := simpleSigning{}
	if err := json.Unmarshal(payload, &ss); err != nil {
		return fmt.Errorf("payload is not simple-signing JSON: %w", err)
	}
	if ss.Critical.Type != cosignPayloadType {
		return fmt.Errorf("payload type is %q", ss.Critical.Type)
	}
	if ss.Critical.Image.DockerManifestDigest != digest {
		return fmt.Errorf("payload signs %s, not %s", ss.Critical.Image.DockerManifestDigest, digest)
	}
	return nil
}

// verifies checks sig over payload with any key, using each key type's
// cosign default: SHA-256 with ECDSA (ASN.1) or RSA PKCS#1 v1.5, raw ed25519.
func (v *Verifier) verifies(payload, sig []byte) bool {
	sum := sha256.Sum256(payload)
	for _, key := range v.keys {
		switch k := key.(type) {
		case *ecdsa.PublicKey:
			if ecdsa.VerifyASN1(k, sum[:], sig) {
				return true
			}
		case *rsa.PublicKey:
			if rsa.VerifyPKCS1v15(k, crypto.SHA256, sum[:], sig) == nil {
				return true
			}
		case ed25519.PublicKey:
			if ed25519.Verify(k, payload, sig) {
				return true
			}
		}
	}
	return false
}
