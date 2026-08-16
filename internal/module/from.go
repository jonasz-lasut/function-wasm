package module

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/crossplane/crossplane-runtime/v2/pkg/fieldpath"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// FromComposite materialises the *From fields of src: the named field of the
// composite resource (its object form, as observed) is read and must decode
// into the matching source — an OCISource object for ociFrom, an HTTPSource
// object for httpFrom, a string for pathFrom. The returned source is concrete
// and can be resolved; a src without *From fields is returned unchanged.
// composite may be nil when nothing is to be read from it.
func FromComposite(src v1beta1.ModuleSource, composite map[string]any) (v1beta1.ModuleSource, error) {
	if err := Validate(src); err != nil {
		return src, err
	}
	var name, from string
	var into any
	switch {
	case src.OCIFrom != "":
		name, from = "ociFrom", src.OCIFrom
		into = &src.OCI
	case src.HTTPFrom != "":
		name, from = "httpFrom", src.HTTPFrom
		into = &src.HTTP
	case src.PathFrom != "":
		name, from = "pathFrom", src.PathFrom
		into = &src.Path
	default:
		return src, nil
	}
	if composite == nil {
		return src, fmt.Errorf("module.%s %s: no observed composite resource to read it from", name, from)
	}
	value, err := fieldpath.Pave(composite).GetValue(from)
	if err != nil {
		return src, fmt.Errorf("module.%s: cannot read %s from the composite resource: %w", name, from, err)
	}
	if err := decodeStrict(value, into); err != nil {
		return src, fmt.Errorf("module.%s: %s of the composite resource is not a %s: %w", name, from, kindOf(name), err)
	}
	src.OCIFrom, src.HTTPFrom, src.PathFrom = "", "", ""
	if err := Validate(src); err != nil {
		return src, fmt.Errorf("module.%s: %s of the composite resource: %w", name, from, err)
	}
	return src, nil
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

func kindOf(name string) string {
	switch name {
	case "ociFrom":
		return "{ref, digest, credentials} object"
	case "httpFrom":
		return "{url, digest} object"
	default:
		return "string"
	}
}
