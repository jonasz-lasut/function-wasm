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
	"net/url"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
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
	// Verifier holds the cosign public keys a required module is checked
	// against. When RequireSignature is nil it is also the requirement: set,
	// every module must be an OCI artifact carrying a signature it accepts and
	// http and path sources are refused (the all-or-nothing --cosign-key a
	// runtime without --sandbox-policy-file has); unset, nothing is verified.
	Verifier *Verifier
	// RequireSignature, when set, decides per normalized module location
	// whether a cosign signature is required, replacing Verifier's
	// all-or-nothing with the operator policy's per-repository decision
	// (internal/authz). A required OCI module is verified with Verifier's keys,
	// or refused when no key is configured; a required http source is refused
	// (it cannot carry a cosign signature); a path source is never required,
	// being operator-controlled. Nil keeps the all-or-nothing behaviour above,
	// so a runtime without an operator policy behaves exactly as before.
	RequireSignature func(location string) bool
}

// A Ref is a resolved module: the digest that pins it and how to fetch it.
type Ref struct {
	// Digest pins the module and keys the caches, sha256:<hex>: the manifest
	// digest of an OCI artifact (the manifest names the layer's digest, and
	// the layer is the module), otherwise the module's own content digest.
	Digest string
	// Description names the source for logs and error messages.
	Description string

	fetch    func(ctx context.Context) ([]byte, error)
	verify   func(ctx context.Context) error
	manifest func(ctx context.Context) ([]byte, bool, error)
}

// Fetch returns the module bytes, verified along the chain Digest pins.
func (r *Ref) Fetch(ctx context.Context) ([]byte, error) {
	return r.fetch(ctx)
}

// Manifest returns the module manifest an OCI artifact carries as its
// manifest layer (internal/manifest.LayerMediaType), verified along the same
// chain as the module, or found false: a source without one — every path and
// http source, an artifact pushed without a manifest — has nothing to
// declare and runs as it always did.
func (r *Ref) Manifest(ctx context.Context) ([]byte, bool, error) {
	if r.manifest == nil {
		return nil, false, nil
	}
	return r.manifest(ctx)
}

// Verify checks the module's signature when one is required — the resolver's
// all-or-nothing --cosign-key, or an operator policy's per-repository
// requirement (Options.RequireSignature) — and is a no-op otherwise. A
// required module with no key configured to verify it is refused here too. It
// is a precondition of running the module — not of fetching it — so it must be
// called before any cache is consulted: a compiled artifact on disk may
// predate the key, and a signature is only known to be good for the lifetime
// of the process that checked it.
func (r *Ref) Verify(ctx context.Context) error {
	if r.verify == nil {
		return nil
	}
	return r.verify(ctx)
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

// Validate reports whether src names exactly one usable source: Type is set
// and exactly one of the object it names (oci, http, path) or From is set,
// with no object of another type present. A source read from the composite
// resource (From) is validated for its field path here and for its content
// by FromComposite.
func Validate(src v1beta1.ModuleSource) error {
	if src.Type == "" {
		return errors.New("module.type is required: OCI, HTTP or Path")
	}
	objects := map[v1beta1.ModuleType]bool{
		v1beta1.ModuleTypeOCI:  src.OCI != nil,
		v1beta1.ModuleTypeHTTP: src.HTTP != nil,
		v1beta1.ModuleTypePath: src.Path != "",
	}
	if _, ok := objects[src.Type]; !ok {
		return fmt.Errorf("module.type %q must be OCI, HTTP or Path", src.Type)
	}
	for _, t := range []v1beta1.ModuleType{v1beta1.ModuleTypeOCI, v1beta1.ModuleTypeHTTP, v1beta1.ModuleTypePath} {
		if t != src.Type && objects[t] {
			return fmt.Errorf("module.%s is set but module.type is %s", fieldOf(t), src.Type)
		}
	}
	hasObject := objects[src.Type]
	hasFrom := src.From != ""
	if hasObject == hasFrom {
		return fmt.Errorf("module.type %s needs exactly one of module.%s and module.from", src.Type, fieldOf(src.Type))
	}
	if src.From != "" && !fromPattern.MatchString(src.From) {
		return fmt.Errorf("module.from %q must be a field under spec or status of the composite resource, e.g. status.module", src.From)
	}
	if src.OCI != nil {
		if src.OCI.Ref == "" {
			return errors.New("module.oci.ref is required")
		}
		if _, err := ociLocation(src.OCI.Ref); err != nil {
			return err
		}
	}
	if src.HTTP != nil {
		if src.HTTP.URL == "" {
			return errors.New("module.http.url is required")
		}
		if _, err := httpLocation(src.HTTP.URL); err != nil {
			return err
		}
		if err := checkDigest("module.http.digest", src.HTTP.Digest); err != nil {
			return err
		}
	}
	return nil
}

// repositorySegment is one path component of an OCI repository name (the
// distribution spec's grammar): lowercase, so "..", "." and empty components
// — which a registry or a proxy might collapse, escaping a repository
// prefix — are not repository names.
var repositorySegment = regexp.MustCompile(`^[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*$`)

// ociLocation checks an OCI reference and returns "registry/repository" —
// what the compositionPolicy's pullModule permits are matched against —
// without the tag or digest.
func ociLocation(ref string) (string, error) {
	d, err := name.NewDigest(ref)
	if err != nil {
		return "", fmt.Errorf("module.oci.ref %q must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported", ref)
	}
	repo := d.Context().RepositoryStr()
	for seg := range strings.SplitSeq(repo, "/") {
		if !repositorySegment.MatchString(seg) {
			return "", fmt.Errorf("module.oci.ref %q: repository %q is not a valid repository name (lowercase path components, no . or .. or empty ones)", ref, repo)
		}
	}
	return d.Context().RegistryStr() + "/" + repo, nil
}

// httpLocation checks a module URL and returns "scheme://host/path" —
// what the compositionPolicy's pullModule permits are matched against —
// with the host lowercased and the path required to be normalized, so a prefix
// cannot be escaped with dot segments the server would collapse; the query
// is not part of the location.
func httpLocation(raw string) (string, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return "", fmt.Errorf("module.http.url %q is not a URL: %w", raw, err)
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return "", fmt.Errorf("module.http.url %q must be an http or https URL", raw)
	}
	if u.Hostname() == "" || u.User != nil {
		return "", fmt.Errorf("module.http.url %q must name a host and carry no user information", raw)
	}
	if !egress.NormalizedPath(u.Path) {
		return "", fmt.Errorf("module.http.url %q must have a normalized path (no . or .. segments, no empty segments)", raw)
	}
	return u.Scheme + "://" + strings.ToLower(u.Host) + u.Path, nil
}

// fieldOf names the Input field holding a source of type t.
func fieldOf(t v1beta1.ModuleType) string {
	switch t {
	case v1beta1.ModuleTypeOCI:
		return "oci"
	case v1beta1.ModuleTypeHTTP:
		return "http"
	case v1beta1.ModuleTypePath:
		return "path"
	}
	return string(t)
}

func checkDigest(field, digest string) error {
	if digest == "" {
		return fmt.Errorf("%s is required: the sha256 of the module file (sha256sum fn.wasm)", field)
	}
	if !digestPattern.MatchString(digest) {
		return fmt.Errorf("%s %q is not sha256:<64 hex characters>", field, digest)
	}
	return nil
}

// Resolve resolves a concrete src — one whose From field has been
// materialised by FromComposite. auth authenticates OCI pulls; nil falls back
// to the resolver's keychain. Resolving does no I/O: the digest comes from
// the Input (or, for a served file, from the file), and fetching is deferred
// to Ref.Fetch, which the caller only invokes when no compiled artifact of
// the module is at hand.
func (r *Resolver) Resolve(ctx context.Context, src v1beta1.ModuleSource, auth authn.Authenticator) (*Ref, error) {
	if err := Validate(src); err != nil {
		return nil, err
	}
	if src.From != "" {
		return nil, errors.New("a module.from source must be materialised with FromComposite before it is resolved")
	}
	// Whether this module must carry a cosign signature is settled before it is
	// resolved: the legacy all-or-nothing --cosign-key requires every module
	// (and refuses non-OCI), while an operator policy requires it per
	// repository. A required http source is refused here (it cannot carry a
	// cosign signature); the OCI path wires Verify to the crypto or, when a
	// policy requires a signature but no key is configured, to a refusal.
	location, err := locationOf(src)
	if err != nil {
		return nil, err
	}
	required := r.signatureRequired(src.Type, location)
	if required && src.Type != v1beta1.ModuleTypeOCI {
		return nil, r.nonOCISignatureRefusal(src.Type, location)
	}
	switch src.Type {
	case v1beta1.ModuleTypePath:
		return r.resolvePath(src)
	case v1beta1.ModuleTypeHTTP:
		return r.resolveHTTP(src)
	case v1beta1.ModuleTypeOCI:
		return r.resolveOCI(ctx, src, auth, required)
	}
	panic("unreachable: Validate rejects other types")
}

// locationOf returns the normalized location of a concrete source - what a
// repository policy matches against - or empty for a path source, which has
// none. Validate has already passed, so ociLocation/httpLocation succeed.
func locationOf(src v1beta1.ModuleSource) (string, error) {
	switch src.Type {
	case v1beta1.ModuleTypeOCI:
		return ociLocation(src.OCI.Ref)
	case v1beta1.ModuleTypeHTTP:
		return httpLocation(src.HTTP.URL)
	case v1beta1.ModuleTypePath:
		return "", nil
	}
	return "", nil
}

// signatureRequired reports whether a module at location must carry a cosign
// signature. With an operator policy (RequireSignature set) the requirement is
// per repository: a path source, being operator-controlled and locationless,
// is never required. Without one it is the legacy all-or-nothing --cosign-key:
// a configured Verifier requires every module, no key requires none.
func (r *Resolver) signatureRequired(t v1beta1.ModuleType, location string) bool {
	if r.opts.RequireSignature != nil {
		if t == v1beta1.ModuleTypePath {
			return false
		}
		return r.opts.RequireSignature(location)
	}
	return r.opts.Verifier != nil
}

// nonOCISignatureRefusal explains why a required non-OCI source cannot run:
// only OCI artifacts carry a cosign signature. The legacy all-or-nothing keeps
// its exact wording; a policy names the location it refused (only an http
// source can reach here, a path source is never required under a policy).
func (r *Resolver) nonOCISignatureRefusal(t v1beta1.ModuleType, location string) error {
	if r.opts.RequireSignature == nil {
		return errors.New("only cosign-signed oci modules are accepted (--cosign-key is set); http and path sources are refused")
	}
	return fmt.Errorf("module.%s %q requires a cosign signature (operator policy), but only OCI modules can be signature-verified", fieldOf(t), location)
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
