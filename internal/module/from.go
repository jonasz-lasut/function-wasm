package module

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/crossplane/crossplane-runtime/v2/pkg/fieldpath"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// FromComposite materialises src.From: the named field of the composite
// resource (its object form, as observed) is read and must decode into the
// source src.Type names — an OCISource object for OCI, an HTTPSource object
// for HTTP, a string for Path. The returned source is concrete and can be
// resolved; a src without From is returned unchanged. composite may be nil
// when nothing is to be read from it.
//
// policy fences what the composite resource may choose and is read from the
// Input only, never from the composite: an XR-chosen ref (or url) must start
// with one of policy.repositoryAllowList's prefixes when the list is set, and
// an XR-chosen OCI object may name a pipeline-step credential only when
// policy.credentialsAllowList lists it — the credential belongs to the
// Composition, the registry host would be the XR author's, and a registry
// that answers with a Basic challenge receives the secret. Without such a
// policy, modules the XR chooses are pulled with the runtime's own Docker
// config or anonymously. A static source is not subject to policy; its shape
// is still validated.
func FromComposite(src v1beta1.ModuleSource, policy *v1beta1.Policy, composite map[string]any) (v1beta1.ModuleSource, error) {
	if err := Validate(src); err != nil {
		return src, err
	}
	if err := ValidatePolicy(policy); err != nil {
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
	return src, admit(from, src, policy)
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
