# Targets for the pdb-addon guest, an ABI v2 component from mainline Go.
# `guestfn` and the runtime come from this repository (cargo run), so nothing
# needs to be installed except Go, Rust and, for render, the crossplane CLI
# and Docker (the render engine runs in a container; the platform's
# go-templating step too); render-check also needs xprin. wit-bindgen 0.62.0
# is only needed to regenerate the checked-in bindings (make gen-bindings;
# the root Makefile's gen-bindings installs it pinned and mirrors the result
# into the go template). GUESTFN, FUNCTION_BIN (read by ../render.sh), XPRIN
# and WIT_BINDGEN can name prebuilt binaries instead, as CI does.
ROOT := $(abspath ../..)
MODULE := github.com/jonasz-lasut/function-wasm/examples/pdb-addon
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --
WIT_BINDGEN ?= wit-bindgen

.PHONY: build lint test gen-bindings render render-check clean

build: ## Compile the guest to fn.wasm (a component: go build, the world embedded, the wasip1 adapter linked)
	$(GUESTFN) build --dir $(CURDIR)

# internal/bindings is wit-bindgen's output: its pointer arithmetic over the
# wasm linear memory is what vet's unsafeptr check exists to flag, so vet
# takes the guest's own packages; .golangci.yml keeps golangci-lint off the
# generated files the same way.
lint: ## Vet the guest's own packages and run golangci-lint over it
	go vet $$(go list ./... | grep -v /internal/bindings/)
	golangci-lint run ./...

test: ## Run the guest's unit tests natively
	go test ./...

# wit-bindgen overwrites its own files and leaves export_wit_world/run.go,
# the hand-written slot, alone.
gen-bindings: ## Regenerate internal/bindings from wit/ (needs wit-bindgen 0.62.0)
	$(WIT_BINDGEN) go wit -w function --pkg-name $(MODULE)/internal/bindings --out-dir internal/bindings

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -f fn.wasm
