package module

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/crossplane/crossplane-runtime/v2/pkg/fieldpath"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/authz"
)

// FromComposite materialises src.From: the named field of the composite
// resource (its object form, as observed) is read and must decode into the
// source src.Type names — an OCISource object for OCI, an HTTPSource object
// for HTTP, a string for Path. The returned source is concrete and can be
// resolved; a src without From is returned unchanged. composite may be nil
// when nothing is to be read from it.
//
// comp is the Input's compositionPolicy, compiled - read from the Input only,
// never from the composite: an XR-chosen ref (or url) must be permitted by a
// pullModule rule, and an XR-chosen OCI object may name a pipeline-step
// credential only where a spendCredential rule permits it for that repository
// — the credential belongs to the Composition, the registry host would be the
// XR author's, and a registry that answers with a Basic challenge receives
// the secret. Without such permits, modules the XR chooses are refused (no
// policy at all) or pulled with the runtime's own Docker config or
// anonymously (no credentials named). A static source is not subject to the
// fence; its shape is still validated. The policy's principal (namespace,
// xrKind) is read from the composite resource itself.
func FromComposite(src v1beta1.ModuleSource, comp *authz.CompositionPolicy, composite map[string]any) (v1beta1.ModuleSource, error) {
	if err := Validate(src); err != nil {
		return src, err
	}
	if src.From == "" {
		return src, nil
	}
	from := src.From
	var into any
	switch src.Type {
	case v1beta1.ModuleTypeOCI:
		into = &src.OCI
	case v1beta1.ModuleTypeHTTP:
		into = &src.HTTP
	case v1beta1.ModuleTypePath:
		into = &src.Path
	}
	if composite == nil {
		return src, fmt.Errorf("module.from %s: no observed composite resource to read it from", from)
	}
	value, err := fieldpath.Pave(composite).GetValue(from)
	if err != nil {
		return src, fmt.Errorf("module.from: cannot read %s from the composite resource: %w", from, err)
	}
	if err := decodeStrict(value, into); err != nil {
		return src, fmt.Errorf("module.from: %s of the composite resource is not a %s: %w", from, kindOf(src.Type), err)
	}
	src.From = ""
	if err := Validate(src); err != nil {
		return src, fmt.Errorf("module.from: %s of the composite resource: %w", from, err)
	}
	return src, admit(from, src, comp, principalFromComposite(composite))
}

// principalFromComposite reads the caller identity a composition policy may
// key on from the observed composite resource: its kind and namespace. A nil
// composite yields the zero principal, which matches no principal condition -
// safe, since an unmatched permit denies.
func principalFromComposite(composite map[string]any) authz.Principal {
	p := authz.Principal{}
	if composite == nil {
		return p
	}
	p.XRKind, _ = composite["kind"].(string)
	if md, ok := composite["metadata"].(map[string]any); ok {
		p.Namespace, _ = md["namespace"].(string)
	}
	return p
}

// decodeStrict casts an unstructured value into a source through JSON,
// refusing unknown fields so a typo in the composite resource is an error
// rather than an ignored field.
func decodeStrict(value any, into any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(into); err != nil {
		return err
	}
	if _, err := dec.Token(); err == nil {
		return errors.New("trailing data")
	}
	return nil
}

func kindOf(t v1beta1.ModuleType) string {
	switch t {
	case v1beta1.ModuleTypeOCI:
		return "{ref, credentials} object"
	case v1beta1.ModuleTypeHTTP:
		return "{url, digest} object"
	case v1beta1.ModuleTypePath:
		return "string"
	}
	return string(t)
}
