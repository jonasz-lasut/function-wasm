package wasmfn

import (
	"encoding/json"
	"fmt"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
)

// GetConfig decodes the config field of the function-wasm Input that invoked
// the guest into v, which is typically a pointer to the guest's own config
// struct. It returns false when the Input carries no config.
func GetConfig(req *fnv1.RunFunctionRequest, v any) (bool, error) {
	cfg, ok := req.GetInput().GetFields()["config"]
	if !ok {
		return false, nil
	}
	raw, err := cfg.MarshalJSON()
	if err != nil {
		return true, fmt.Errorf("cannot encode input.config: %w", err)
	}
	if err := json.Unmarshal(raw, v); err != nil {
		return true, fmt.Errorf("cannot decode input.config: %w", err)
	}
	return true, nil
}
