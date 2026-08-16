package module

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
)

// Credential keys accepted from a pipeline-step Secret.
const (
	keyDockerConfigJSON = ".dockerconfigjson"
	keyUsername         = "username"
	keyPassword         = "password"
)

// AuthFor builds a registry authenticator for ref from the data of a
// pipeline-step credential: a kubernetes.io/dockerconfigjson Secret
// (.dockerconfigjson) or a Secret with username and password keys.
func AuthFor(ref string, data map[string][]byte) (authn.Authenticator, error) {
	if cfg, ok := data[keyDockerConfigJSON]; ok {
		return authFromDockerConfig(ref, cfg)
	}
	user, password := data[keyUsername], data[keyPassword]
	if len(user) == 0 || len(password) == 0 {
		return nil, fmt.Errorf("credential has neither a %s key nor %s and %s keys", keyDockerConfigJSON, keyUsername, keyPassword)
	}
	return &authn.Basic{Username: string(user), Password: string(password)}, nil
}

type dockerConfig struct {
	Auths map[string]dockerAuth `json:"auths"`
}

type dockerAuth struct {
	Auth     string `json:"auth"`
	Username string `json:"username"`
	Password string `json:"password"`
}

// authFromDockerConfig picks the auths entry for ref's registry. Docker Hub
// is stored under its legacy v1 URL, and entries may carry a scheme.
func authFromDockerConfig(ref string, raw []byte) (authn.Authenticator, error) {
	parsed, err := name.ParseReference(ref)
	if err != nil {
		return nil, fmt.Errorf("cannot parse reference: %w", err)
	}
	registry := parsed.Context().RegistryStr()

	cfg := dockerConfig{}
	if err := json.Unmarshal(raw, &cfg); err != nil {
		return nil, fmt.Errorf("cannot parse %s: %w", keyDockerConfigJSON, err)
	}
	candidates := []string{registry, "https://" + registry, "http://" + registry, "https://" + registry + "/v1/"}
	if registry == name.DefaultRegistry {
		candidates = append(candidates, "https://index.docker.io/v1/", "index.docker.io", "docker.io")
	}
	for _, c := range candidates {
		a, ok := cfg.Auths[c]
		if !ok {
			continue
		}
		user, password := a.Username, a.Password
		if a.Auth != "" {
			decoded, err := base64.StdEncoding.DecodeString(a.Auth)
			if err != nil {
				return nil, fmt.Errorf("cannot decode auth for %s: %w", c, err)
			}
			user, password, _ = strings.Cut(string(decoded), ":")
		}
		if user == "" && password == "" {
			return nil, fmt.Errorf("auth entry for %s carries no credentials", c)
		}
		return &authn.Basic{Username: user, Password: password}, nil
	}
	return nil, errors.New("no auth entry for registry " + registry)
}
