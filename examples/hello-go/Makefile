# Targets for the hello guest. `guestfn` and the runtime come from this
# repository (cargo run), so nothing needs to be installed except Go, Rust
# and, for render, the crossplane CLI and Docker (the render engine runs in
# a container).
ROOT := $(abspath ../..)

.PHONY: build test render render-check clean

build: ## Compile the guest to fn.wasm
	cd $(ROOT) && cargo run --release -p guestfn -- build --dir $(CURDIR)

test: ## Run the guest's unit tests natively
	go test ./...

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -f fn.wasm
