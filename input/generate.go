//go:build generate
// +build generate

// Remove existing and generate new input manifests
//go:generate rm -rf ../package/input/
//go:generate go run -tags generate sigs.k8s.io/controller-tools/cmd/controller-gen paths=./v1beta1 object crd:crdVersions=v1 output:artifacts:config=../package/input

package input

import (
	_ "sigs.k8s.io/controller-tools/cmd/controller-gen" //nolint:typecheck
)
