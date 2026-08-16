// Package v1beta1 contains the input type for this Function.
// +kubebuilder:object:generate=true
// +groupName=wasm.fn.crossplane.io
// +versionName=v1beta1
package v1beta1

import (
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
)

// This isn't a custom resource, in the sense that we never install its CRD.
// It is a KRM-like object, so we generate a CRD to describe its schema.

// Input configures which WebAssembly module function-wasm runs and what it
// receives.
// +kubebuilder:object:root=true
// +kubebuilder:storageversion
// +kubebuilder:resource:categories=crossplane
type Input struct {
	metav1.TypeMeta `json:",inline"`

	metav1.ObjectMeta `json:"metadata,omitempty"`

	// Module locates the WebAssembly module to run.
	Module ModuleSource `json:"module"`

	// Config is passed to the module verbatim as part of its request input.
	// The runtime does not interpret it; a Go guest reads it with
	// wasmfn.GetConfig.
	// +optional
	// +kubebuilder:pruning:PreserveUnknownFields
	Config *runtime.RawExtension `json:"config,omitempty"`
}

// ModuleSource locates a module. Exactly one of OCI, HTTP, Path, OCIFrom,
// HTTPFrom and PathFrom must be set.
type ModuleSource struct {
	// OCI pulls the module from an OCI registry.
	// +optional
	OCI *OCISource `json:"oci,omitempty"`

	// HTTP downloads the module from a URL.
	// +optional
	HTTP *HTTPSource `json:"http,omitempty"`

	// Path names a module file relative to the directory the function was
	// started with (--module-dir); it is refused when that flag is unset.
	// Meant for local rendering and volume-mounted modules; it carries no
	// digest.
	// +optional
	Path string `json:"path,omitempty"`

	// OCIFrom names a field of the observed composite resource, under spec
	// or status, that holds an OCI source — an object with ref, digest and
	// optionally credentials — e.g. "status.module". The value is read on
	// every request, so each composite resource can pick its own module.
	// +optional
	// +kubebuilder:validation:Pattern=`^(spec|status)\..+`
	OCIFrom string `json:"ociFrom,omitempty"`

	// HTTPFrom names a field of the observed composite resource, under spec
	// or status, that holds an HTTP source — an object with url and digest.
	// +optional
	// +kubebuilder:validation:Pattern=`^(spec|status)\..+`
	HTTPFrom string `json:"httpFrom,omitempty"`

	// PathFrom names a field of the observed composite resource, under spec
	// or status, that holds a module path (a string) relative to --module-dir.
	// +optional
	// +kubebuilder:validation:Pattern=`^(spec|status)\..+`
	PathFrom string `json:"pathFrom,omitempty"`
}

// OCISource is a module stored as an OCI artifact: a manifest whose single
// layer (media type application/wasm or a vnd.wasm content layer) is the
// module, as produced by guestfn push or oras push. A tar layer holding a
// .wasm file (a FROM scratch image) is accepted too.
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
	// config (DOCKER_CONFIG) and anonymous access are tried.
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
