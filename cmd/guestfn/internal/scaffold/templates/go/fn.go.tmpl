package main

import (
	"context"
	"io"
	"net/http"
	"strings"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/resource/composed"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/pkg/wasmfn"
)

// Config is what the Composition passes under input.config.
type Config struct {
	// Greeting is written into the ConfigMap; defaults to "hello".
	Greeting string `json:"greeting,omitempty"`
	// GreetingURL, when set, is fetched through the host and its body used
	// as the greeting — the manifest's requires.egress grant decides
	// whether the request is allowed.
	GreetingURL string `json:"greetingUrl,omitempty"`
}

// Function composes a ConfigMap greeting the composite resource.
type Function struct {
	fnv1.UnimplementedFunctionRunnerServiceServer

	log logging.Logger
	// http performs requests through the host (wasmfn.HTTPClient()); a
	// native test injects an httptest client instead.
	http *http.Client
}

// RunFunction adds a ConfigMap named after the composite resource to the
// desired state.
func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	f.log.Info("Running function", "tag", req.GetMeta().GetTag())
	rsp := response.To(req, response.DefaultTTL)

	cfg := Config{Greeting: "hello"}
	if _, err := wasmfn.GetConfig(req, &cfg); err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot read config"))
		return rsp, nil
	}
	if cfg.GreetingURL != "" {
		greeting, err := f.fetchGreeting(ctx, cfg.GreetingURL)
		if err != nil {
			response.Fatal(rsp, errors.Wrap(err, "cannot fetch greeting"))
			return rsp, nil
		}
		cfg.Greeting = greeting
	}

	xr, err := request.GetObservedCompositeResource(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot get observed composite resource from %T", req))
		return rsp, nil
	}
	desired, err := request.GetDesiredComposedResources(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot get desired composed resources from %T", req))
		return rsp, nil
	}

	cm := composed.New()
	cm.SetAPIVersion("v1")
	cm.SetKind("ConfigMap")
	if err := cm.SetString("data.greeting", cfg.Greeting+" "+xr.Resource.GetName()); err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot set greeting"))
		return rsp, nil
	}
	desired["greeting"] = &resource.DesiredComposed{Resource: cm}

	if err := response.SetDesiredComposedResources(rsp, desired); err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot set desired composed resources in %T", rsp))
		return rsp, nil
	}
	response.Normal(rsp, "greeted "+xr.Resource.GetName())
	response.ConditionTrue(rsp, "FunctionSuccess", "Success").TargetCompositeAndClaim()
	return rsp, nil
}

// fetchGreeting GETs url through the host and returns the body of a 200,
// trimmed; a request the host refuses surfaces as *wasmfn.HTTPError.
func (f *Function) fetchGreeting(ctx context.Context, url string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return "", err
	}
	rsp, err := f.http.Do(req)
	if err != nil {
		return "", err
	}
	defer rsp.Body.Close() //nolint:errcheck // Nothing to do about a close error on a fully read body.
	body, err := io.ReadAll(rsp.Body)
	if err != nil {
		return "", err
	}
	if rsp.StatusCode != http.StatusOK {
		return "", errors.Errorf("GET %s: status %d", url, rsp.StatusCode)
	}
	return strings.TrimSpace(string(body)), nil
}
