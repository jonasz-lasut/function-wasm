package main

import (
	"context"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
)

// What a platform step composes for a WebApp named web: the add-on reads the
// Deployment and must leave all three as they are.
const (
	xrJSON         = `{"apiVersion":"platform.example.org/v1alpha1","kind":"WebApp","metadata":{"name":"web","namespace":"default"},"spec":{"image":"nginx:1.29","replicas":3,"port":8080,"addOn":"fn.wasm"}}`
	deploymentJSON = `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":{"matchLabels":{"app":"web"}},"template":{"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"web","image":"nginx:1.29","ports":[{"containerPort":8080}]}]}}}}`
	serviceJSON    = `{"apiVersion":"v1","kind":"Service","spec":{"selector":{"app":"web"},"ports":[{"port":8080,"targetPort":8080}]}}`
	budgetJSON     = `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"maxUnavailable":1,"selector":{"matchLabels":{"app":"web"}}}}`
)

// state is a State with the WebApp as its composite and the given composed
// resources, keyed by composition resource name.
func state(resources map[string]string) *fnv1.State {
	s := &fnv1.State{Composite: &fnv1.Resource{Resource: resource.MustStructJSON(xrJSON)}}
	if len(resources) > 0 {
		s.Resources = map[string]*fnv1.Resource{}
	}
	for name, j := range resources {
		s.Resources[name] = &fnv1.Resource{Resource: resource.MustStructJSON(j)}
	}
	return s
}

// input is the function-wasm Input of the add-on step, with config.
func input(config string) *structpb.Struct {
	return resource.MustStructJSON(`{"apiVersion":"wasm.fn.crossplane.io/v1","kind":"Input","module":{"type":"Path","from":"spec.addOn","allowEmpty":true},"config":` + config + `}`)
}

func TestRunFunction(t *testing.T) {
	meta := &fnv1.ResponseMeta{Tag: "web", Ttl: durationpb.New(60 * time.Second)}
	// Context an earlier step left for later ones; it must pass through.
	pipelineContext := resource.MustStructJSON(`{"platform.example.org/tier":"standard"}`)

	type args struct {
		req *fnv1.RunFunctionRequest
	}
	type want struct {
		rsp *fnv1.RunFunctionResponse
		err error
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"AddsBudgetForDeployment": {
			reason: "A Deployment of 3 replicas gets a budget over its selector, named after it, with the default maxUnavailable; the XR, the Deployment, the Service and the context pass through untouched.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired:  state(map[string]string{"deployment": deploymentJSON, "service": serviceJSON}),
				Context:  pipelineContext,
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:    meta,
				Desired: state(map[string]string{"deployment": deploymentJSON, "service": serviceJSON, "deployment-pdb": budgetJSON}),
				Context: pipelineContext,
			}},
		},
		"ConfiguredMaxUnavailable": {
			reason: "input.config.maxUnavailable replaces the default.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Input:    input(`{"maxUnavailable":2}`),
				Observed: state(nil),
				Desired:  state(map[string]string{"deployment": deploymentJSON}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment":     deploymentJSON,
					"deployment-pdb": `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"maxUnavailable":2,"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
			}},
		},
		"EveryDeployment": {
			reason: "Every apps/v1 Deployment gets its own budget, and only those: a kind of the same name in another group is not one.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired: state(map[string]string{
					"api":    `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":2,"selector":{"matchLabels":{"app":"api"}}}}`,
					"worker": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":4,"selector":{"matchLabels":{"app":"worker"}}}}`,
					"other":  `{"apiVersion":"example.org/v1","kind":"Deployment","spec":{"replicas":4,"selector":{"matchLabels":{"app":"other"}}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"api":        `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":2,"selector":{"matchLabels":{"app":"api"}}}}`,
					"worker":     `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":4,"selector":{"matchLabels":{"app":"worker"}}}}`,
					"other":      `{"apiVersion":"example.org/v1","kind":"Deployment","spec":{"replicas":4,"selector":{"matchLabels":{"app":"other"}}}}`,
					"api-pdb":    `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"maxUnavailable":1,"selector":{"matchLabels":{"app":"api"}}}}`,
					"worker-pdb": `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"maxUnavailable":1,"selector":{"matchLabels":{"app":"worker"}}}}`,
				}),
			}},
		},
		"WholeSelector": {
			reason: "The budget takes the Deployment's whole selector, so matchExpressions narrow it as they narrow the Deployment.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"track","operator":"In","values":["stable"]}]}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment":     `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"track","operator":"In","values":["stable"]}]}}}`,
					"deployment-pdb": `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"maxUnavailable":1,"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"track","operator":"In","values":["stable"]}]}}}`,
				}),
			}},
		},
		"ReadyOnceObserved": {
			reason: "A budget that already exists is ready: it has no Ready condition for Crossplane to wait on.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(map[string]string{"deployment": deploymentJSON, "deployment-pdb": budgetJSON}),
				Desired:  state(map[string]string{"deployment": deploymentJSON}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: &fnv1.State{
					Composite: &fnv1.Resource{Resource: resource.MustStructJSON(xrJSON)},
					Resources: map[string]*fnv1.Resource{
						"deployment":     {Resource: resource.MustStructJSON(deploymentJSON)},
						"deployment-pdb": {Resource: resource.MustStructJSON(budgetJSON), Ready: fnv1.Ready_READY_TRUE},
					},
				},
			}},
		},
		"SkipsSingleReplica": {
			reason: "A Deployment of one replica gets no budget, and a warning says why.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":1,"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":1,"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_WARNING,
					Message:  `no PodDisruptionBudget for Deployment "deployment": it runs 1 replica(s), and a budget cannot keep a lone pod available without blocking node drains`,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"ReplicasDefaultToOne": {
			reason: "A Deployment without spec.replicas runs one, as the API server defaults it, so it gets no budget either.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_WARNING,
					Message:  `no PodDisruptionBudget for Deployment "deployment": it runs 1 replica(s), and a budget cannot keep a lone pod available without blocking node drains`,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"SkipsDeploymentWithoutSelector": {
			reason: "A Deployment with an empty selector gets no budget: an empty one would cover every pod in the namespace.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":{}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":{}}}`,
				}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_WARNING,
					Message:  `no PodDisruptionBudget for Deployment "deployment": it has no label selector to copy, and a budget with an empty one would cover every pod in the namespace`,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"KeepsWhatEarlierStepsNamed": {
			reason: "A resource an earlier step composed under the budget's name is left as it is, with a warning.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired: state(map[string]string{
					"deployment":     deploymentJSON,
					"deployment-pdb": `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"minAvailable":2,"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment":     deploymentJSON,
					"deployment-pdb": `{"apiVersion":"policy/v1","kind":"PodDisruptionBudget","spec":{"minAvailable":2,"selector":{"matchLabels":{"app":"web"}}}}`,
				}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_WARNING,
					Message:  `no PodDisruptionBudget for Deployment "deployment": an earlier step already composed a resource named "deployment-pdb", which is left as it is`,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"NothingToProtect": {
			reason: "Without a Deployment in the desired state the response is the request's desired state and context, unchanged.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "web"},
				Observed: state(nil),
				Desired:  state(map[string]string{"service": serviceJSON}),
				Context:  pipelineContext,
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:    meta,
				Desired: state(map[string]string{"service": serviceJSON}),
				Context: pipelineContext,
			}},
		},
		"ConfigNotAnInteger": {
			reason: "A maxUnavailable that is not an integer is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:    &fnv1.RequestMeta{Tag: "web"},
				Input:   input(`{"maxUnavailable":"one"}`),
				Desired: state(map[string]string{"deployment": deploymentJSON}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:    meta,
				Desired: state(map[string]string{"deployment": deploymentJSON}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Message:  "cannot read config: cannot decode input.config: json: cannot unmarshal string into Go struct field Config.maxUnavailable of type int",
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"ConfigBelowOne": {
			reason: "A maxUnavailable below 1 is a fatal result: such a budget blocks every node drain.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:    &fnv1.RequestMeta{Tag: "web"},
				Input:   input(`{"maxUnavailable":0}`),
				Desired: state(map[string]string{"deployment": deploymentJSON}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:    meta,
				Desired: state(map[string]string{"deployment": deploymentJSON}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Message:  "config.maxUnavailable must be at least 1, got 0: a budget that allows no disruption blocks every node drain",
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"ReplicasNotAnInteger": {
			reason: "A Deployment whose spec.replicas is not an integer is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta: &fnv1.RequestMeta{Tag: "web"},
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":"three","selector":{"matchLabels":{"app":"web"}}}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":"three","selector":{"matchLabels":{"app":"web"}}}}`,
				}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Message:  `cannot read the replicas of Deployment "deployment": spec.replicas: not a (int64) number`,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
		"SelectorNotAnObject": {
			reason: "A Deployment whose spec.selector is not a label selector is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta: &fnv1.RequestMeta{Tag: "web"},
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":"app=web"}}`,
				}),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: meta,
				Desired: state(map[string]string{
					"deployment": `{"apiVersion":"apps/v1","kind":"Deployment","spec":{"replicas":3,"selector":"app=web"}}`,
				}),
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Message:  `cannot read the selector of Deployment "deployment": spec.selector: not an object`,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			f := &Function{log: logging.NewNopLogger()}
			rsp, err := f.RunFunction(context.Background(), tc.args.req)

			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.err, err, cmpopts.EquateErrors()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want err, +got err:\n%s", tc.reason, diff)
			}
		})
	}
}
