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

// Input configures which WebAssembly module function-wasm runs, what an
// XR-chosen module may be and spend, what a run may consume, and what the
// module receives.
// +kubebuilder:object:root=true
// +kubebuilder:storageversion
// +kubebuilder:resource:categories=crossplane
// +kubebuilder:validation:XValidation:rule="!has(self.module.from) || self.module.type == 'Path' || (has(self.policy) && has(self.policy.repositoryAllowList) && size(self.policy.repositoryAllowList) > 0)",message="module.from with type OCI or HTTP requires policy.repositoryAllowList"
type Input struct {
	metav1.TypeMeta `json:",inline"`

	metav1.ObjectMeta `json:"metadata,omitempty"`

	// Module locates the WebAssembly module to run.
	Module ModuleSource `json:"module"`

	// Policy fences a module the composite resource chooses (module.from):
	// which repositories it may come from and which step credentials it may
	// spend. It is read from the Input only — never through module.from — so
	// an XR author may pick the module, not widen the policy. It is ignored
	// for a module the Composition names statically.
	// +optional
	Policy *Policy `json:"policy,omitempty"`

	// Limits narrow what this step's run may consume below the runtime's
	// ceilings (--module-timeout, --module-memory-limit). Asking for more
	// than a ceiling is a fatal result naming both. Read from the Input only.
	// +optional
	Limits *Limits `json:"limits,omitempty"`

	// Sandbox grants the module filesystem, egress or environment access
	// beyond the default sandbox (nothing), each within a ceiling the
	// operator set with an --enable-sandbox-* flag; a grant outside the
	// ceiling is a fatal result naming the grant and the flag. Read from the
	// Input only.
	// +optional
	Sandbox *Sandbox `json:"sandbox,omitempty"`

	// Config is passed to the module verbatim as part of its request input.
	// The runtime does not interpret it; a Go guest reads it with
	// wasmfn.GetConfig.
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
	// composite resource can pick its own module; the Input's policy fences
	// what it may pick and spend. Nothing but the source is read from the
	// composite resource: policy, limits and sandbox are the Composition's.
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
	// through module.from may name credentials only when the Input's
	// policy.credentialsAllowList lists them (and its ref passes
	// policy.repositoryAllowList): the composite resource's author would
	// otherwise choose the registry host a step credential is sent to.
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

// Policy fences what a module chosen by the composite resource (module.from)
// may be and spend. It only applies to such modules; a source the Composition
// names statically is trusted as the Composition is, and the policy is
// ignored for it. Never read from the composite resource.
// +kubebuilder:validation:XValidation:rule="!has(self.credentialsAllowList) || size(self.credentialsAllowList) == 0 || (has(self.repositoryAllowList) && size(self.repositoryAllowList) > 0)",message="policy.credentialsAllowList requires policy.repositoryAllowList"
type Policy struct {
	// RepositoryAllowList are string prefixes an XR-chosen oci.ref (or
	// http.url) must start with, e.g. "ghcr.io/example-org/" — the trailing
	// slash matters: "ghcr.io/example-org" also admits
	// "ghcr.io/example-organisation/...". Prefixes are matched against the
	// normalized location — registry/repository for OCI (no tag or digest),
	// scheme://host/path for HTTP (host lowercased, no query) — and a ref or
	// URL whose path is not already normalized (dot segments, empty
	// segments) is refused outright. A ref outside every prefix is a fatal
	// result naming the policy and the ref. Required whenever module.from
	// names an OCI or HTTP source: without it the composite resource's
	// author could point the runtime at any host and read what its answer
	// says.
	// +optional
	// +kubebuilder:validation:items:MinLength=1
	RepositoryAllowList []string `json:"repositoryAllowList,omitempty"`

	// CredentialsAllowList are the pipeline-step credentials an XR-chosen
	// oci object may name, spent only on a ref RepositoryAllowList admits —
	// so it requires RepositoryAllowList: a credential must never be
	// spendable on an arbitrary host. Absent or empty, an XR-chosen object
	// naming credentials is refused.
	// +optional
	// +kubebuilder:validation:items:MinLength=1
	CredentialsAllowList []string `json:"credentialsAllowList,omitempty"`
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

	// Instructions caps the number of wasm instructions one run may
	// execute (wasmtime fuel); at most --module-instruction-limit. The
	// count is deterministic across nodes and runs. Requires the runtime
	// to be started with --enable-fuel; without it the field is refused.
	// +optional
	// +kubebuilder:validation:Minimum=1
	Instructions *int64 `json:"instructions,omitempty"`

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

// Sandbox grants a module access beyond the default sandbox - nothing but
// the request. The operator sets the ceiling with runtime flags, the
// Composition asks for what its module needs, the module gets the
// intersection; a grant outside the ceiling is a fatal result before the
// module runs (docs/one-pager-sandbox.md is the design). Never read from
// the composite resource.
type Sandbox struct {
	// Filesystem grants a private /tmp. Host directories are deliberately
	// not mountable into a module: the request is a module's only view of
	// the world beyond what it may write for itself.
	// +optional
	Filesystem *SandboxFilesystem `json:"filesystem,omitempty"`

	// Egress grants HTTP(S) requests through the host to listed hosts.
	// +optional
	Egress *SandboxEgress `json:"egress,omitempty"`

	// Env sets the environment variables the module sees - exactly these,
	// never the runtime's (--enable-sandbox-env). Each entry names one
	// variable with a literal value or a reference to a step credential's
	// key. A name set twice (across Env and every EnvFrom import) is
	// refused.
	// +optional
	Env []EnvVar `json:"env,omitempty"`

	// EnvFrom bulk-imports every key of a step credential as an environment
	// variable, optionally with a prefix. A key that is not a valid
	// variable name (after prefixing) refuses the run - use Env with
	// valueFrom to select specific keys instead.
	// +optional
	EnvFrom []EnvFromSource `json:"envFrom,omitempty"`
}

// EnvVar sets one environment variable from a literal or a step credential.
// +kubebuilder:validation:XValidation:rule="has(self.value) != has(self.valueFrom)",message="exactly one of value and valueFrom must be set"
type EnvVar struct {
	// Name of the variable: an identifier, [A-Za-z_][A-Za-z0-9_]*.
	// +kubebuilder:validation:Pattern=`^[A-Za-z_][A-Za-z0-9_]*$`
	Name string `json:"name"`

	// Value is a literal string. Non-secret configuration only.
	// +optional
	Value *string `json:"value,omitempty"`

	// ValueFrom reads the value from a source in the request.
	// +optional
	ValueFrom *ValueSource `json:"valueFrom,omitempty"`
}

// ValueSource reads a value from the request. Exactly one member is set;
// new kinds (composite, context) are added as members - never a break.
// +kubebuilder:validation:XValidation:rule="has(self.credential)",message="exactly one source must be set (credential)"
type ValueSource struct {
	// Credential reads one key of a pipeline-step credential.
	// +optional
	Credential *CredentialKeyRef `json:"credential,omitempty"`
}

// CredentialKeyRef selects one key of a step credential.
type CredentialKeyRef struct {
	// Name of the step credential.
	// +kubebuilder:validation:MinLength=1
	Name string `json:"name"`
	// Key within the credential's data.
	// +kubebuilder:validation:MinLength=1
	Key string `json:"key"`
}

// EnvFromSource bulk-imports a step credential's keys as environment
// variables. Exactly one source member is set.
// +kubebuilder:validation:XValidation:rule="has(self.credential)",message="exactly one source must be set (credential)"
type EnvFromSource struct {
	// Credential names the step credential whose keys become variables.
	// +optional
	Credential *CredentialRef `json:"credential,omitempty"`

	// Prefix is prepended to each imported key, e.g. "VAULT_" turns
	// "TOKEN" into "VAULT_TOKEN". Must be a valid identifier prefix
	// ([A-Za-z_][A-Za-z0-9_]*) or empty.
	// +optional
	// +kubebuilder:validation:Pattern=`^([A-Za-z_][A-Za-z0-9_]*)?$`
	Prefix string `json:"prefix,omitempty"`
}

// CredentialRef names a step credential.
type CredentialRef struct {
	// Name of the step credential.
	// +kubebuilder:validation:MinLength=1
	Name string `json:"name"`
}

// SandboxFilesystem is what a module gets of a filesystem beyond nothing: a
// private, empty, writable /tmp for the duration of the request. Host
// directories are not mountable - that boundary stays closed.
type SandboxFilesystem struct {
	// PrivateTmp pre-opens a private, empty, writable /tmp for the duration
	// of the request, created under the runtime's $TMPDIR before the module
	// runs and removed when the run ends whatever its outcome - systemd's
	// PrivateTmp (--enable-sandbox-private-tmp).
	// +optional
	PrivateTmp bool `json:"privateTmp,omitempty"`
}

// SandboxEgress is the egress a module gets: HTTP(S) requests performed by
// the host on the guest's behalf (the wasmfn.http import), never raw sockets.
type SandboxEgress struct {
	// HTTP lists the requests the host will perform for the guest; anything
	// not matched by an entry is refused. The operator's
	// --enable-sandbox-egress and --sandbox-egress-policy (allowed hosts,
	// blocked CIDRs, per-request budgets) are the ceiling.
	// +optional
	HTTP []SandboxHTTPRule `json:"http,omitempty"`
}

// SandboxHTTPRule admits requests to one host or host pattern.
// +kubebuilder:validation:XValidation:rule="has(self.host) != has(self.hostPattern)",message="exactly one of host and hostPattern must be set"
type SandboxHTTPRule struct {
	// Host is an exact host name, e.g. api.example.com. Exactly one of Host
	// and HostPattern is set.
	// +optional
	// +kubebuilder:validation:MinLength=1
	Host string `json:"host,omitempty"`

	// HostPattern is a host name with a leading wildcard label, e.g.
	// "*.internal.example.com".
	// +optional
	// +kubebuilder:validation:Pattern=`^\*\.[^*]+$`
	HostPattern string `json:"hostPattern,omitempty"`

	// Methods the rule admits, e.g. [GET, POST]; at least one — nothing is
	// admitted implicitly.
	// +kubebuilder:validation:MinItems=1
	// +kubebuilder:validation:items:Enum=GET;HEAD;POST;PUT;PATCH;DELETE;OPTIONS
	Methods []string `json:"methods"`

	// PathPrefix the request path must start with, e.g. /v1/; empty admits
	// any path.
	// +optional
	// +kubebuilder:validation:Pattern=`^/`
	PathPrefix string `json:"pathPrefix,omitempty"`
}
