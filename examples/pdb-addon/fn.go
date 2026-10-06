package main

import (
	"context"
	"maps"
	"slices"

	"github.com/crossplane/crossplane-runtime/v2/pkg/fieldpath"
	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/resource/composed"
	"github.com/crossplane/function-sdk-go/response"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"

	"github.com/jonasz-lasut/function-wasm/examples/pdb-addon/internal/wasmfn"
)

// Config is what the Composition passes under input.config.
type Config struct {
	// MaxUnavailable is how many of a Deployment's pods a voluntary
	// disruption (a node drain, an eviction) may take down at once.
	MaxUnavailable int `json:"maxUnavailable"`
}

// defaultMaxUnavailable lets drains proceed one pod at a time.
const defaultMaxUnavailable = 1

// minReplicas is the smallest Deployment a budget can protect: a lone pod
// cannot stay available through a drain, so a budget over it either lets the
// pod go anyway or, written as minAvailable, blocks every node drain.
const minReplicas = 2

// budgetSuffix names each budget after the composed Deployment it guards.
const budgetSuffix = "-pdb"

// Function adds a PodDisruptionBudget for every Deployment an earlier
// pipeline step composed.
type Function struct {
	fnv1.UnimplementedFunctionRunnerServiceServer

	log logging.Logger
}

// RunFunction adds the budgets to the desired state earlier steps built and
// leaves everything they composed as it was.
func (f *Function) RunFunction(_ context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	f.log.Debug("Running function", "tag", req.GetMeta().GetTag())
	// The response starts as the request's desired state and context, so
	// whatever this function does not add passes through untouched.
	rsp := response.To(req, response.DefaultTTL)

	cfg := Config{MaxUnavailable: defaultMaxUnavailable}
	if _, err := wasmfn.GetConfig(req, &cfg); err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot read config"))
		return rsp, nil
	}
	// The manifest's config schema says the same, but a module served
	// without its manifest (a Path source) gets no schema check.
	if cfg.MaxUnavailable < 1 {
		response.Fatal(rsp, errors.Errorf("config.maxUnavailable must be at least 1, got %d: a budget that allows no disruption blocks every node drain", cfg.MaxUnavailable))
		return rsp, nil
	}

	desired, err := request.GetDesiredComposedResources(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot get desired composed resources from %T", req))
		return rsp, nil
	}

	budgets := map[resource.Name]*resource.DesiredComposed{}
	// Sorted, so the warnings come out in the same order every reconcile.
	for _, name := range slices.Sorted(maps.Keys(desired)) {
		d := desired[name].Resource
		if d.GetAPIVersion() != "apps/v1" || d.GetKind() != "Deployment" {
			continue
		}
		budget := name + budgetSuffix
		if _, taken := desired[budget]; taken {
			response.Warning(rsp, errors.Errorf("no PodDisruptionBudget for Deployment %q: an earlier step already composed a resource named %q, which is left as it is", name, budget))
			continue
		}
		replicas, err := replicasOf(d)
		if err != nil {
			response.Fatal(rsp, errors.Wrapf(err, "cannot read the replicas of Deployment %q", name))
			return rsp, nil
		}
		if replicas < minReplicas {
			response.Warning(rsp, errors.Errorf("no PodDisruptionBudget for Deployment %q: it runs %d replica(s), and a budget cannot keep a lone pod available without blocking node drains", name, replicas))
			continue
		}
		selector, err := selectorOf(d)
		if err != nil {
			response.Fatal(rsp, errors.Wrapf(err, "cannot read the selector of Deployment %q", name))
			return rsp, nil
		}
		if selector == nil {
			response.Warning(rsp, errors.Errorf("no PodDisruptionBudget for Deployment %q: it has no label selector to copy, and a budget with an empty one would cover every pod in the namespace", name))
			continue
		}
		// A budget is in effect as soon as it exists and reports no Ready
		// condition for Crossplane to wait on, so it is ready once observed.
		ready := resource.ReadyUnspecified
		if _, exists := req.GetObserved().GetResources()[string(budget)]; exists {
			ready = resource.ReadyTrue
		}
		budgets[budget] = &resource.DesiredComposed{Resource: newBudget(selector, cfg.MaxUnavailable), Ready: ready}
		f.log.Debug("Adding a PodDisruptionBudget", "deployment", string(name), "budget", string(budget), "maxUnavailable", cfg.MaxUnavailable)
	}

	if len(budgets) == 0 {
		return rsp, nil
	}
	if err := response.SetDesiredComposedResources(rsp, budgets); err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot set desired composed resources in %T", rsp))
		return rsp, nil
	}
	return rsp, nil
}

// replicasOf reads spec.replicas the way the API server defaults it: a
// Deployment that leaves it out runs one replica.
func replicasOf(d *composed.Unstructured) (int64, error) {
	replicas, err := d.GetInteger("spec.replicas")
	if fieldpath.IsNotFound(err) {
		return 1, nil
	}
	return replicas, err
}

// selectorOf returns the Deployment's spec.selector, the pods the budget must
// cover: the whole label selector, so matchExpressions narrow the budget as
// they narrow the Deployment. It is nil when the selector matches by nothing.
func selectorOf(d *composed.Unstructured) (map[string]any, error) {
	v, err := d.GetValue("spec.selector")
	if fieldpath.IsNotFound(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	selector, ok := v.(map[string]any)
	if !ok {
		return nil, errors.Errorf("spec.selector: not an object")
	}
	labels, _ := selector["matchLabels"].(map[string]any)
	expressions, _ := selector["matchExpressions"].([]any)
	if len(labels) == 0 && len(expressions) == 0 {
		return nil, nil
	}
	return selector, nil
}

// newBudget is a policy/v1 PodDisruptionBudget over the pods selector
// matches. It carries no name: Crossplane names composed resources itself.
func newBudget(selector map[string]any, maxUnavailable int) *composed.Unstructured {
	return &composed.Unstructured{Unstructured: unstructured.Unstructured{Object: map[string]any{
		"apiVersion": "policy/v1",
		"kind":       "PodDisruptionBudget",
		"spec": map[string]any{
			"maxUnavailable": int64(maxUnavailable),
			"selector":       selector,
		},
	}}}
}
