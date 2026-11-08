# Targets for the function-oci-go guest, the hello function as an OCI module
# (docs/oci/modules.md): Go builds and tests it natively, Docker
# builds the image, and guestfn, from this repository (cargo run), publishes
# the module - the image with the manifest appended as a layer - into the
# image's repository. REGISTRY is where: by default the local registry:3 of
# the OCI runtime's development loop (docs/oci/install.md), which
# needs a Linux machine with /dev/kvm to render against. GUESTFN can name a
# prebuilt binary instead, as CI does.
ROOT := $(abspath ../..)
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --
REGISTRY ?= 127.0.0.1:5000
VERSION := $(shell sed -nE 's/^version: *([^ #]+).*$$/\1/p' guestfn.yaml)
IMAGE := $(REGISTRY)/function-oci-go:v$(VERSION)-image
MODULE := $(REGISTRY)/function-oci-go:v$(VERSION)

.PHONY: build test lint push clean

build: ## Build the image for this machine's platform
	docker build -t $(IMAGE) .

test: ## Run the unit tests natively
	go test ./...

lint: ## Vet and lint the guest
	go vet ./...
	golangci-lint run ./...

push: build ## Push the image, publish the module into its repository and print the Composition's module block
	docker push $(IMAGE)
	$(GUESTFN) push $(MODULE) --manifest guestfn.yaml --image "$$(docker inspect --format '{{index .RepoDigests 0}}' $(IMAGE))"

clean: ## Remove the image built here
	docker rmi $(IMAGE) 2>/dev/null || true
