package sandbox

import (
	"fmt"
	"strings"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
)

// Sources are where env bindings resolve values from.
type Sources struct {
	// Credentials are the request's step credentials.
	Credentials map[string]*fnv1.Credentials
	// Withheld is the name of the pull credential, refused as a source
	// (invariant 3: what the guest may not see in its request, it may not
	// see in its environ either).
	Withheld string
}

// Materialize resolves a module's env bindings - its manifest's requires.env,
// already admitted by the policy layers - against the request's credentials
// and returns the resolved environment map. It is called after the manifest
// is read; the bindings are shape-valid (ValidateBindings ran with the
// manifest).
//
// Invariants enforced here:
//   - the pull credential is refused as a source
//   - a missing credential or key is a fatal-worthy error
//   - a NUL byte in a resolved value is refused (WASI limitation)
func Materialize(bindings []EnvBinding, src Sources) (map[string]string, error) {
	if len(bindings) == 0 {
		return nil, nil
	}
	env := make(map[string]string, len(bindings))
	for i, b := range bindings {
		field := fmt.Sprintf("requires.env[%d] (%s)", i, b.Name)
		v, err := resolveCredential(field, b.FromCredential.Name, b.FromCredential.Key, src)
		if err != nil {
			return nil, err
		}
		if strings.IndexByte(v, 0) >= 0 {
			return nil, fmt.Errorf("%s: the value of %s contains a NUL byte, which WASI cannot pass", field, b.Name)
		}
		env[b.Name] = v
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
