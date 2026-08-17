package sandbox

import (
	"fmt"
	"slices"
	"strings"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// Sources are where valueFrom resolves values from.
type Sources struct {
	// Credentials are the request's step credentials.
	Credentials map[string]*fnv1.Credentials
	// Withheld is the name of the pull credential, refused as a source
	// (invariant 3: what the guest may not see in its request, it may not
	// see in its environ either).
	Withheld string
}

// Materialize resolves the Input's sandbox.env and sandbox.envFrom against
// the request's credentials and returns the resolved environment map. It is
// called after registryAuth, the first point where the pull credential's
// name is known. Shape validation (Validate) and the ceiling check (Grant)
// have already run.
//
// Invariants enforced here:
//   - the pull credential is refused as a source
//   - a missing credential or key is a fatal-worthy error
//   - a NUL byte in a resolved value is refused (WASI limitation)
//   - an envFrom key that is not a valid variable name (after prefixing)
//     refuses the run
//   - a name set twice (across env and envFrom) is refused
func Materialize(s *v1beta1.Sandbox, src Sources) (map[string]string, error) {
	if s == nil {
		return nil, nil
	}
	if len(s.Env) == 0 && len(s.EnvFrom) == 0 {
		return nil, nil
	}

	env := make(map[string]string, len(s.Env))
	seen := make(map[string]string, len(s.Env)) // name -> origin field

	// env[]: literal values and valueFrom references.
	for i, e := range s.Env {
		field := fmt.Sprintf("sandbox.env[%d]", i)
		if e.Value != nil {
			env[e.Name] = *e.Value
			seen[e.Name] = field
			continue
		}
		// valueFrom - shape is validated, Credential is non-nil.
		v, err := resolveCredential(field+".valueFrom.credential", e.ValueFrom.Credential.Name, e.ValueFrom.Credential.Key, src)
		if err != nil {
			return nil, err
		}
		if strings.IndexByte(v, 0) >= 0 {
			return nil, fmt.Errorf("%s: the value of %s contains a NUL byte, which WASI cannot pass", field, e.Name)
		}
		env[e.Name] = v
		seen[e.Name] = field
	}

	// envFrom[]: bulk import every key of a credential.
	for i, ef := range s.EnvFrom {
		field := fmt.Sprintf("sandbox.envFrom[%d]", i)
		data, err := credentialData(field, ef.Credential.Name, src)
		if err != nil {
			return nil, err
		}
		// Sort keys for deterministic error messages.
		keys := make([]string, 0, len(data))
		for k := range data {
			keys = append(keys, k)
		}
		slices.Sort(keys)
		for _, k := range keys {
			name := ef.Prefix + k
			if !ValidEnvKey(name) {
				return nil, fmt.Errorf("%s: credential %q has key %q, which is not an environment variable name; name the keys you need in sandbox.env instead", field, ef.Credential.Name, name)
			}
			if prev, ok := seen[name]; ok {
				return nil, fmt.Errorf("%s: %s is already set by %s", field, name, prev)
			}
			v := string(data[k])
			if strings.IndexByte(v, 0) >= 0 {
				return nil, fmt.Errorf("%s: the value of %s contains a NUL byte, which WASI cannot pass", field, name)
			}
			env[name] = v
			seen[name] = field
		}
	}

	return env, nil
}

// resolveCredential reads one key of a step credential.
func resolveCredential(field, credName, key string, src Sources) (string, error) {
	data, err := credentialData(field, credName, src)
	if err != nil {
		return "", err
	}
	v, ok := data[key]
	if !ok {
		return "", fmt.Errorf("%s: credential %q has no key %q", field, credName, key)
	}
	return string(v), nil
}

// credentialData returns the data map of a named step credential.
func credentialData(field, credName string, src Sources) (map[string][]byte, error) {
	if credName == src.Withheld {
		return nil, fmt.Errorf("%s: credential %q is the pull credential and cannot be used as a source", field, credName)
	}
	cred, ok := src.Credentials[credName]
	if !ok {
		return nil, fmt.Errorf("%s: the request carries no credential %q; declare it on the pipeline step", field, credName)
	}
	cd := cred.GetCredentialData()
	if cd == nil {
		return nil, fmt.Errorf("%s: credential %q has no data", field, credName)
	}
	return cd.GetData(), nil
}
