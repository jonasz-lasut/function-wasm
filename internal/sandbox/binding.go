package sandbox

import "fmt"

// EnvBinding binds one environment variable to one key of a pipeline-step
// credential - the shape a module manifest's requires.env carries
// (docs/one-pager-three-layer-authz.md, the env model). A binding is a
// requirement the module declares, never a grant: the credential value still
// arrives at the pipeline step, and both Cedar layers must permit setEnv and
// spendCredential before Materialize resolves it. Literal values are
// deliberately not expressible - non-secret configuration is the Input's
// config, read by the guest through wasmfn.GetConfig.
type EnvBinding struct {
	// Name of the variable the guest sees: an identifier,
	// [A-Za-z_][A-Za-z0-9_]*.
	Name string `json:"name"`
	// FromCredential selects the step credential key that supplies the value.
	FromCredential CredentialKey `json:"fromCredential"`
}

// CredentialKey selects one key of a step credential.
type CredentialKey struct {
	// Name of the step credential.
	Name string `json:"name"`
	// Key within the credential's data.
	Key string `json:"key"`
}

// ValidateBindings checks the shape of env bindings - a module manifest's
// requires.env - naming a wrong one as field[i]: an identifier name set at
// most once, and a credential name and key.
func ValidateBindings(field string, bindings []EnvBinding) error {
	seen := make(map[string]string, len(bindings))
	for i, b := range bindings {
		entry := fmt.Sprintf("%s[%d]", field, i)
		if !ValidEnvKey(b.Name) {
			return fmt.Errorf("%s.name %q is not an identifier ([A-Za-z_][A-Za-z0-9_]*)", entry, b.Name)
		}
		if b.FromCredential.Name == "" {
			return fmt.Errorf("%s.fromCredential.name must not be empty", entry)
		}
		if b.FromCredential.Key == "" {
			return fmt.Errorf("%s.fromCredential.key must not be empty", entry)
		}
		if prev, ok := seen[b.Name]; ok {
			return fmt.Errorf("%s: %s is already bound by %s", entry, b.Name, prev)
		}
		seen[b.Name] = entry
	}
	return nil
}
