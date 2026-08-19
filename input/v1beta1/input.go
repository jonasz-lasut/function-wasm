// Package v1beta1 contains the input type for this Function.
// +kubebuilder:object:generate=true
// +groupName=wasm.fn.crossplane.io
// +versionName=v1beta1
package v1beta1

import (
	"k8s.io/apimachinery/pkg/api/resource"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
)

// This isn't a custom resource, in the sense that we never install its CRD.
// It is a KRM-like object, so we generate a CRD to describe its schema.

// Input configures which WebAssembly module function-wasm runs, what the
// composition author's own policy layer permits, what a run may consume, and
// what the module receives. What a module may do beyond the default sandbox
// is decided per capability by three AND-combined layers
// (docs/one-pager-three-layer-authz.md): the module's manifest requests it,
// the Input's compositionPolicy permits it, and the operator's
// --sandbox-policy-file permits it.
// +kubebuilder:object:root=true
// +kubebuilder:storageversion
// +kubebuilder:resource:categories=crossplane
// +kubebuilder:validation:XValidation:rule="!has(self.module.from) || self.module.type == 'Path' || (has(self.compositionPolicy) && size(self.compositionPolicy) > 0)",message="module.from with type OCI or HTTP requires a compositionPolicy permitting pullModule"
type Input struct {
	metav1.TypeMeta `json:",inline"`

	metav1.ObjectMeta `json:"metadata,omitempty"`

	// Module locates the WebAssembly module to run.
	Module ModuleSource `json:"module"`

	// CompositionPolicy is the composition author's own Cedar policy layer,
	// as policy text over the same schema as the operator's
	// --sandbox-policy-file (actions pullModule, spendCredential,
	// grantEgress, usePrivateTmp, setEnv; a Request principal carrying
	// namespace and xrKind; Repository, HostPattern, Capability and
	// Credential entities). It is AND-combined with the module's manifest
	// and the operator's policy, so it can only narrow. Two regimes: a
	// sandbox action it scopes no rule for is not narrowed (the operator and
	// the manifest decide alone), while a module the composite resource
	// chooses (module.from) is refused unless a pullModule permit matches
	// its repository - and may spend a step credential only where a
	// spendCredential permit matches. Read from the Input only - never
	// through module.from - so an XR author may pick the module, not widen
	// the policy. Malformed Cedar is a fatal result at admission.
	// +optional
	CompositionPolicy string `json:"compositionPolicy,omitempty"`

	// Limits narrow what this step's run may consume below the runtime's
	// ceilings (--module-timeout, --module-memory-limit). Asking for more
	// than a ceiling is a fatal result naming both. Read from the Input only.
	// +optional
	Limits *Limits `json:"limits,omitempty"`

	// Config is passed to the module verbatim as part of its request input.
	// The runtime does not interpret it; a Go guest reads it with
	// wasmfn.GetConfig. Non-secret module configuration belongs here - the
	// module's environment is its manifest's requires.env credential
	// bindings, never Input-authored values.
	// +optional
	// +kubebuilder:pruning:PreserveUnknownFields
	Config *runtime.RawExtension `json:"config,omitempty"`
}

// ModuleType names the kind of source a module comes from.
// +kubebuilder:validation:Enum=OCI;HTTP;Path
type ModuleType string

// The module source kinds.
const (
	// ModuleTypeOCI is an artifact in an OCI registry (the oci object).
	ModuleTypeOCI ModuleType = "OCI"
	// ModuleTypeHTTP is a file served over HTTP(S) (the http object).
	ModuleTypeHTTP ModuleType = "HTTP"
	// ModuleTypePath is a file under the runtime's --module-dir (path).
	ModuleTypePath ModuleType = "Path"
)

// ModuleSource locates a module: Type says what kind of source it is, and
// exactly one of the typed object Type names (oci, http, path) or From — the
// field of the observed composite resource holding that object — is set.
// The CEL rules below let schema-aware tooling (crossplane beta validate,
// admission with the CRD installed) reject a Composition that sets none,
// several or a mismatched object before it reconciles; the runtime checks the
// same at every request.
// +kubebuilder:validation:XValidation:rule="self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)",message="type OCI needs exactly one of oci and from, and oci is only allowed with type OCI"
// +kubebuilder:validation:XValidation:rule="self.type == 'HTTP' ? (has(self.http) != has(self.from)) : !has(self.http)",message="type HTTP needs exactly one of http and from, and http is only allowed with type HTTP"
// +kubebuilder:validation:XValidation:rule="self.type == 'Path' ? (has(self.path) != has(self.from)) : !has(self.path)",message="type Path needs exactly one of path and from, and path is only allowed with type Path"
type ModuleSource struct {
	// Type is the kind of source: OCI (an artifact in a registry, the oci
	// object), HTTP (a URL, the http object) or Path (a file under the
	// runtime's --module-dir, path). The Composition always chooses the
	// kind; with From, the composite resource chooses the instance.
	Type ModuleType `json:"type"`

	// OCI pulls the module from an OCI registry. Set with type OCI, unless
	// From names the composite resource field holding the object.
	// +optional
	OCI *OCISource `json:"oci,omitempty"`

	// HTTP downloads the module from a URL. Set with type HTTP, unless From
	// names the composite resource field holding the object.
	// +optional
	HTTP *HTTPSource `json:"http,omitempty"`

	// Path names a module file relative to the directory the function was
	// started with (--module-dir); it is refused when that flag is unset.
	// Meant for local rendering and volume-mounted modules; it carries no
	// digest. Set with type Path, unless From names the composite resource
	// field holding the string.
	// +optional
	Path string `json:"path,omitempty"`

	// From names a field of the observed composite resource, under spec or
	// status, that holds the source Type names — an {ref, credentials}
	// object for OCI, an {url, digest} object for HTTP, a string for Path —
	// e.g. "status.module". The value is read on every request, so each
	// composite resource can pick its own module; the Input's
	// compositionPolicy fences what it may pick (pullModule) and spend
	// (spendCredential). Nothing but the source is read from the composite
	// resource: compositionPolicy and limits are the Composition's.
	// +optional
	// +kubebuilder:validation:Pattern=`^(spec|status)\..+`
	From string `json:"from,omitempty"`
}

// OCISource is a module stored as an OCI artifact: a manifest whose single
// layer (media type application/wasm or a vnd.wasm content layer) is the
// module, as produced by guestfn push or oras push. A tar layer (a FROM
// scratch image) is accepted when it holds the module at exactly /fn.wasm.
type OCISource struct {
	// Ref is the artifact reference pinned to its manifest digest,
	// registry/repository@sha256:<hex>, as guestfn push prints it. The
	// manifest digest pins the module: the manifest names its layer's
	// digest and every fetch is verified along that chain, so it also
	// addresses the module in the caches. A tag alone is not accepted: tags
	// can be moved, and the runtime resolves nothing at request time.
	// registry/repository:tag@sha256:<hex> is fine — the digest is what is
	// fetched, the tag is human-readable context.
	// +kubebuilder:validation:Pattern=`^[^@\s]+@sha256:[a-f0-9]{64}$`
	Ref string `json:"ref"`

	// Credentials names a credential of the pipeline step (a Secret) used to
	// pull the artifact. The Secret carries either a .dockerconfigjson key
	// or username and password keys. Without it the function's own Docker
	// config (DOCKER_CONFIG) and anonymous access are tried. An object read
	// through module.from may name credentials only where the Input's
	// compositionPolicy permits spendCredential for them on the ref's
	// repository: the composite resource's author would otherwise choose the
	// registry host a step credential is sent to.
	// +optional
	Credentials string `json:"credentials,omitempty"`
}

// HTTPSource is a module served over HTTP(S).
type HTTPSource struct {
	// URL of the module.
	// +kubebuilder:validation:Pattern=`^https?://`
	URL string `json:"url"`

	// Digest is the sha256 of the module, sha256:<hex>; the download is
	// verified against it and it addresses the module in the caches.
	// +kubebuilder:validation:Pattern=`^sha256:[a-f0-9]{64}$`
	Digest string `json:"digest"`
}

// Limits narrow one run's budget below the runtime's ceilings. Each is
// optional; each must be at most the corresponding runtime flag, or the
// request is a fatal result naming both values. Never read from the composite
// resource.
type Limits struct {
	// Timeout is the wall-clock budget of one run, e.g. "5s" or "1m30s"; at
	// most --module-timeout. The request context's deadline still applies if
	// shorter.
	// +optional
	// +kubebuilder:validation:Type=string
	// +kubebuilder:validation:Pattern=`^([0-9]+(\.[0-9]+)?(ns|us|µs|ms|s|m|h))+$`
	Timeout *metav1.Duration `json:"timeout,omitempty"`

	// Memory caps the guest's linear memory, e.g. "128Mi"; at most
	// --module-memory-limit. A guest that grows past it sees memory.grow
	// fail (a Go guest panics), which ends the run as a fatal result.
	// +optional
	Memory *resource.Quantity `json:"memory,omitempty"`

	// Concurrency caps how many runs of this step execute at once, across
	// all requests. A further request waits for a slot under its own
	// context; when the deadline passes first, it is a fatal result that
	// consumed nothing and is not counted as a run. Keyed by the module's
	// content digest, so two Compositions using the same module share the
	// limit. A value above --max-concurrent-runs is silently capped. No
	// ceiling flag: this only narrows.
	// +optional
	// +kubebuilder:validation:Minimum=1
	Concurrency *int32 `json:"concurrency,omitempty"`
}
