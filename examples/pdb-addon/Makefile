# Targets for the pdb-addon guest. `guestfn` and the runtime come from this
# repository (cargo run), so nothing needs to be installed except Go, Rust
# and, for render, the crossplane CLI and Docker (the render engine runs in
# a container; the platform's go-templating step too); render-check also
# needs xprin. GUESTFN, FUNCTION_BIN (read by ../render.sh) and XPRIN can
# name prebuilt binaries instead, as CI does.
ROOT := $(abspath ../..)
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --

.PHONY: build lint test render render-check clean

build: ## Compile the guest to fn.wasm
	$(GUESTFN) build --dir $(CURDIR)

lint: ## Vet the guest and run golangci-lint over it
	go vet ./...
	golangci-lint run ./...

test: ## Run the guest's unit tests natively
	go test ./...

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -f fn.wasm
