// Package sandbox holds the runtime's sandbox startup checks and the
// materialization of a module's environment from its manifest's env bindings
// (docs/one-pager-sandbox.md, docs/one-pager-three-layer-authz.md). Whether a
// run gets a capability is the three-layer decision in internal/admission -
// the manifest requests, the composition and operator Cedar layers permit;
// the mechanics live in internal/engine, and HTTP egress in internal/egress.
// Host directories are deliberately not mountable into a module - the request
// is a module's only view of the world beyond what it may write for itself -
// so the filesystem capability is the private /tmp alone.
package sandbox

import (
	"fmt"
	"os"
	"regexp"
)

// privateTmpProbePrefix names the throwaway directory NewCeiling creates and
// removes to prove $TMPDIR is writable. It only has to be a valid MkdirTemp
// prefix; the engine names the real per-run directories itself, so this
// validation package need not depend on the CGo engine to reproduce that name.
const privateTmpProbePrefix = "function-wasm-sandbox-probe-"

var envKeyPattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

// ValidEnvKey reports whether key is an environment variable name a guest
// can be given: an identifier, [A-Za-z_][A-Za-z0-9_]*.
func ValidEnvKey(key string) bool {
	return envKeyPattern.MatchString(key)
}

// Options configure the sandbox startup checks for the capabilities the
// operator's Cedar policy can grant.
type Options struct {
	// ProbePrivateTmp asks NewCeiling to prove a private /tmp can be created
	// under $TMPDIR. Set it when the operator policy has a usePrivateTmp rule
	// (OperatorPolicy.HasPrivateTmpRules), so a misconfigured $TMPDIR stops the
	// runtime at startup rather than failing the first request that asks for one.
	ProbePrivateTmp bool
}

// Ceiling marks the sandbox startup checks as passed. Enablement is the
// policy layers' decision (internal/admission), not a flag, so the ceiling
// carries no capability state; it exists so the $TMPDIR probe runs once at
// startup. A nil *Ceiling is safe.
type Ceiling struct{}

// NewCeiling runs the operator's sandbox startup checks once so a misconfigured
// runtime stops instead of failing every request: when a private /tmp can be
// granted (o.ProbePrivateTmp), one is probed under os.TempDir() to make sure it
// can be created there.
func NewCeiling(o Options) (*Ceiling, error) {
	if o.ProbePrivateTmp {
		dir, err := os.MkdirTemp("", privateTmpProbePrefix)
		if err != nil {
			return nil, fmt.Errorf("the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under %s (set TMPDIR to a writable directory): %w", os.TempDir(), err)
		}
		if err := os.Remove(dir); err != nil {
			return nil, fmt.Errorf("the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot remove one under %s: %w", os.TempDir(), err)
		}
	}
	return &Ceiling{}, nil
}
