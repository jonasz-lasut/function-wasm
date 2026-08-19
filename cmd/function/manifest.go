package main

import (
	"github.com/crossplane/function-sdk-go/errors"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
)

// The manifest-load refusal strings live here so RunFunction (fn.go) and
// validate (validate.go) share them and cannot drift: RunFunction reads and
// parses through its per-digest caches and calls Check on every request,
// validate does the same once inline, and both funnel the wrapping through
// these helpers.

// manifestReadError wraps a failure to read a module's manifest layer.
func manifestReadError(err error, desc string) error {
	return errors.Wrapf(err, "cannot read the manifest of module %s", desc)
}

// parseModuleManifest parses a module's raw manifest bytes, returning nil when
// the module carries none (empty bytes). An unparsable manifest refuses the
// module.
func parseModuleManifest(raw []byte, desc string) (*manifest.Manifest, error) {
	if len(raw) == 0 {
		return nil, nil
	}
	m, err := manifest.Parse(raw)
	if err != nil {
		return nil, errors.Wrapf(err, "module %s has an invalid manifest", desc)
	}
	return m, nil
}

// checkManifestGrants holds a parsed manifest (nil when the module has none)
// against what the policy layers granted (AdmitRequires' verdict) - the
// narrowing-only check the runtime makes between load and run: the ABI, the
// minimum runtime, the config against the module's schema, and every
// requirement covered. An unmet requirement or a config outside the module's
// schema refuses the module.
func checkManifestGrants(m *manifest.Manifest, desc string, in *v1beta1.Input, grants manifest.Grants) error {
	if m == nil {
		return nil
	}
	if err := m.Check(grants, in.Config, manifest.RuntimeVersion()); err != nil {
		return errors.Errorf("module %s %v", desc, err)
	}
	return nil
}
