// Package module resolves the ModuleSource of a function-wasm Input to module
// bytes: OCI artifacts, HTTP URLs and files under a served directory, named
// statically or read from a field of the composite resource. Every remote
// module is pinned by a digest the Input states — the manifest digest of an
// OCI reference, the module digest of an HTTP source — every fetch is
// verified against it, and both caches are keyed by it, so resolution itself
// never touches the network.
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
	"github.com/google/go-containerregistry/pkg/name"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// DefaultMaxSize applies when Options.MaxSize is zero.
const DefaultMaxSize = 128 << 20

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
	// Blobs stores fetched blobs — OCI layers as delivered, HTTP modules —
	// on disk by content digest, so each is downloaded once per digest and a
	// restart needs no download. Nil disables the store (tests).
	Blobs *cache.Store
	// HTTPClient is used for HTTP sources and registry access. Nil means a
	// client with a five-minute timeout.
	HTTPClient *http.Client
	// Keychain resolves registry credentials when the Input names none. Nil
	// means authn.DefaultKeychain (Docker config from DOCKER_CONFIG).
	Keychain authn.Keychain
	// Verifier, when set, requires every module to be an OCI artifact
	// carrying a cosign signature it accepts; http and path sources are
	// refused, having no signature to check.
	Verifier *Verifier
}

// A Ref is a resolved module: the digest that pins it and how to fetch it.
type Ref struct {
	// Digest pins the module and keys the caches, sha256:<hex>: the manifest
	// digest of an OCI artifact (the manifest names the layer's digest, and
	// the layer is the module), otherwise the module's own content digest.
	Digest string
	// Description names the source for logs and error messages.
	Description string

	fetch func(ctx context.Context) ([]byte, error)
}

// Fetch returns the module bytes, verified along the chain Digest pins.
func (r *Ref) Fetch(ctx context.Context) ([]byte, error) {
	return r.fetch(ctx)
}

// Resolver resolves module sources. It is safe for concurrent use.
type Resolver struct {
	opts   Options
	client *http.Client
	files  sync.Map // path → fileStamp
}

// NewResolver returns a Resolver.
func NewResolver(o Options) (*Resolver, error) {
	if o.MaxSize <= 0 {
		o.MaxSize = DefaultMaxSize
	}
	if o.Keychain == nil {
		o.Keychain = authn.DefaultKeychain
	}
	client := o.HTTPClient
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Minute}
	}
	return &Resolver{opts: o, client: client}, nil
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
	for field, from := range map[string]string{"ociFrom": src.OCIFrom, "httpFrom": src.HTTPFrom, "pathFrom": src.PathFrom} {
		if from != "" && !fromPattern.MatchString(from) {
			return fmt.Errorf("module.%s %q must be a field under spec or status of the composite resource, e.g. status.module", field, from)
		}
	}
	if src.OCI != nil {
		if src.OCI.Ref == "" {
			return errors.New("module.oci.ref is required")
		}
		if _, err := name.NewDigest(src.OCI.Ref); err != nil {
			return fmt.Errorf("module.oci.ref %q must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported", src.OCI.Ref)
		}
	}
	if src.HTTP != nil {
		if src.HTTP.URL == "" {
			return errors.New("module.http.url is required")
		}
		if err := checkDigest("module.http.digest", src.HTTP.Digest); err != nil {
			return err
		}
	}
	return nil
}

func checkDigest(field, digest string) error {
	if digest == "" {
		return fmt.Errorf("%s is required: the sha256 of the module, as guestfn push prints it", field)
	}
	if !digestPattern.MatchString(digest) {
		return fmt.Errorf("%s %q is not sha256:<64 hex characters>", field, digest)
	}
	return nil
}

// Resolve resolves a concrete src — one whose *From fields have been
// materialised by FromComposite. auth authenticates OCI pulls; nil falls back
// to the resolver's keychain. Resolving does no I/O: the digest comes from
// the Input (or, for a served file, from the file), and fetching is deferred
// to Ref.Fetch, which the caller only invokes when no compiled artifact of
// the module is at hand.
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

// timed wraps a fetch so its duration is observed for source, the source
// kind label of the fetch metric.
func timed(source string, fetch func(ctx context.Context) ([]byte, error)) func(ctx context.Context) ([]byte, error) {
	return func(ctx context.Context) ([]byte, error) {
		start := time.Now()
		defer func() { metrics.FetchDuration.WithLabelValues(source).Observe(time.Since(start).Seconds()) }()
		return fetch(ctx)
	}
}

// verified returns the blob with the given content digest: from the blob
// store when one is configured and holds it, otherwise from fetch, checked
// against digest and saved to the store. what names the blob in the mismatch
// error.
func (r *Resolver) verified(ctx context.Context, what, digest string, fetch func(ctx context.Context) ([]byte, error)) ([]byte, error) {
	if r.opts.Blobs != nil {
		if b, ok := r.opts.Blobs.Get(digest); ok {
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
		return nil, fmt.Errorf("%s content is %s, want %s", what, got, digest)
	}
	if r.opts.Blobs != nil {
		// A full cache is not a reason to fail the request; the blob is
		// simply fetched again next time.
		_ = r.opts.Blobs.Put(digest, b)
	}
	return b, nil
}
