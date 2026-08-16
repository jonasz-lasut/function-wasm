// Package module resolves the ModuleSource of a function-wasm Input to module
// bytes: OCI artifacts, HTTP URLs and files under a served directory, named
// statically or read from a field of the composite resource. Every module is
// identified by its content digest, which is what the compiled-module cache
// is keyed by, and every fetch is verified against it.
package module

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"regexp"
	"sync"
	"time"

	"github.com/google/go-containerregistry/pkg/authn"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// Defaults applied for zero Options fields.
const (
	DefaultMaxSize = 128 << 20
	DefaultTagTTL  = 5 * time.Minute
)

var (
	digestPattern = regexp.MustCompile(`^sha256:[a-f0-9]{64}$`)
	fromPattern   = regexp.MustCompile(`^(spec|status)\..+`)
)

// Options configure a Resolver.
type Options struct {
	// Dir enables Path sources, which are resolved under it. Empty refuses
	// them.
	Dir string
	// MaxSize caps the size of a module in bytes. Zero means DefaultMaxSize.
	MaxSize int64
	// TagTTL is how long an OCI tag's resolution to a digest is reused. Zero
	// means DefaultTagTTL.
	TagTTL time.Duration
	// BlobDir enables a content-addressed on-disk cache of fetched modules
	// (OCI and HTTP), so restarts and registry outages do not need the
	// network. Empty disables it.
	BlobDir string
	// HTTPClient is used for HTTP sources and registry access. Nil means
	// http.DefaultClient with a transport-level default timeout.
	HTTPClient *http.Client
	// Keychain resolves registry credentials when the Input names none. Nil
	// means authn.DefaultKeychain (Docker config from DOCKER_CONFIG).
	Keychain authn.Keychain
	// Now is the clock; nil means time.Now.
	Now func() time.Time
	// Verifier, when set, requires every module to be an OCI artifact
	// carrying a cosign signature it accepts; http and path sources are
	// refused, having no signature to check.
	Verifier *Verifier
}

// A Ref is a resolved module: its content digest and how to fetch it.
type Ref struct {
	// Digest is the module's content digest, sha256:<hex>. Modules with the
	// same digest are the same module wherever they came from.
	Digest string
	// Description names the source for logs and error messages.
	Description string

	fetch func(ctx context.Context) ([]byte, error)
}

// Fetch returns the module bytes, verified against Digest.
func (r *Ref) Fetch(ctx context.Context) ([]byte, error) {
	return r.fetch(ctx)
}

// Resolver resolves module sources. It is safe for concurrent use.
type Resolver struct {
	opts   Options
	client *http.Client
	blobs  *blobStore
	tags   *tagCache
	layers *layerCache
	files  sync.Map // path → fileStamp
}

// NewResolver returns a Resolver.
func NewResolver(o Options) (*Resolver, error) {
	if o.MaxSize <= 0 {
		o.MaxSize = DefaultMaxSize
	}
	if o.TagTTL <= 0 {
		o.TagTTL = DefaultTagTTL
	}
	if o.Now == nil {
		o.Now = time.Now
	}
	if o.Keychain == nil {
		o.Keychain = authn.DefaultKeychain
	}
	client := o.HTTPClient
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Minute}
	}
	r := &Resolver{opts: o, client: client, tags: newTagCache(o.TagTTL, o.Now), layers: newLayerCache()}
	if o.BlobDir != "" {
		blobs, err := newBlobStore(o.BlobDir)
		if err != nil {
			return nil, err
		}
		r.blobs = blobs
	}
	return r, nil
}

// Validate reports whether src names exactly one usable source. Sources read
// from the composite resource (the *From fields) are validated for their
// field path here and for their content by FromComposite.
func Validate(src v1beta1.ModuleSource) error {
	set := 0
	for _, ok := range []bool{src.OCI != nil, src.HTTP != nil, src.Path != "", src.OCIFrom != "", src.HTTPFrom != "", src.PathFrom != ""} {
		if ok {
			set++
		}
	}
	if set != 1 {
		return errors.New("module must set exactly one of oci, http, path, ociFrom, httpFrom and pathFrom")
	}
	for name, from := range map[string]string{"ociFrom": src.OCIFrom, "httpFrom": src.HTTPFrom, "pathFrom": src.PathFrom} {
		if from != "" && !fromPattern.MatchString(from) {
			return fmt.Errorf("module.%s %q must be a field under spec or status of the composite resource, e.g. status.module", name, from)
		}
	}
	if src.Digest != "" && !digestPattern.MatchString(src.Digest) {
		return fmt.Errorf("module.digest %q is not sha256:<64 hex characters>", src.Digest)
	}
	if src.OCI != nil && src.OCI.Ref == "" {
		return errors.New("module.oci.ref is required")
	}
	if src.HTTP != nil {
		if src.HTTP.URL == "" {
			return errors.New("module.http.url is required")
		}
		if src.Digest == "" {
			return errors.New("module.digest is required for an http source")
		}
	}
	return nil
}

// Resolve resolves a concrete src — one whose *From fields have been
// materialised by FromComposite. auth authenticates OCI pulls; nil falls back
// to the resolver's keychain.
func (r *Resolver) Resolve(ctx context.Context, src v1beta1.ModuleSource, auth authn.Authenticator) (*Ref, error) {
	if err := Validate(src); err != nil {
		return nil, err
	}
	if src.OCIFrom != "" || src.HTTPFrom != "" || src.PathFrom != "" {
		return nil, errors.New("a *From source must be materialised with FromComposite before it is resolved")
	}
	if r.opts.Verifier != nil && src.OCI == nil {
		return nil, errors.New("only cosign-signed oci modules are accepted (--cosign-key is set); http and path sources are refused")
	}
	switch {
	case src.Path != "":
		return r.resolvePath(src)
	case src.HTTP != nil:
		return r.resolveHTTP(src)
	default:
		return r.resolveOCI(ctx, src, auth)
	}
}

// pin checks a digest the source reports against the one the Input pinned.
func pin(want, got string) (string, error) {
	if want != "" && want != got {
		return "", fmt.Errorf("module.digest %s does not match the source's %s", want, got)
	}
	return got, nil
}

// readCapped reads at most limit bytes and reports when the source held more.
func readCapped(rd io.Reader, limit int64) ([]byte, error) {
	b, err := io.ReadAll(io.LimitReader(rd, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(b)) > limit {
		return nil, fmt.Errorf("module exceeds the size limit of %d bytes", limit)
	}
	return b, nil
}

func digestOf(b []byte) string {
	sum := sha256.Sum256(b)
	return "sha256:" + hex.EncodeToString(sum[:])
}

// verified wraps a fetch so its bytes are checked against digest and, when a
// blob store is configured, served from and saved to it. source names the
// source kind for metrics.
func (r *Resolver) verified(source, digest string, fetch func(ctx context.Context) ([]byte, error)) func(ctx context.Context) ([]byte, error) {
	return func(ctx context.Context) ([]byte, error) {
		start := time.Now()
		defer func() { metrics.FetchDuration.WithLabelValues(source).Observe(time.Since(start).Seconds()) }()
		if r.blobs != nil {
			if b, ok := r.blobs.get(digest); ok {
				metrics.CacheEvents.WithLabelValues(metrics.CacheBlob, metrics.EventHit).Inc()
				return b, nil
			}
			metrics.CacheEvents.WithLabelValues(metrics.CacheBlob, metrics.EventMiss).Inc()
		}
		b, err := fetch(ctx)
		if err != nil {
			return nil, err
		}
		if got := digestOf(b); got != digest {
			return nil, fmt.Errorf("module content is %s, want %s", got, digest)
		}
		if r.blobs != nil {
			r.blobs.put(digest, b)
		}
		return b, nil
	}
}
