# The function-wasm-ts guest in TypeScript, componentized with jco: needs node and
# npm (protoc only to regenerate the checked-in codec). Render needs the
# crossplane CLI and Docker (the composition's first step,
# function-go-templating, runs in a container).
.PHONY: build test render gen-proto clean wit-deps

ROOT := $(abspath ../..)
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --

node_modules: package-lock.json
	npm ci --no-audit --no-fund

build: node_modules wit-deps ## Bundle and componentize the guest to fn.wasm
	npm run build

test: node_modules ## Type-check, then run the unit tests natively under node
	npm test

gen-proto: node_modules ## Regenerate the checked-in protobuf-es codec (protoc)
	npm run gen-proto

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)


clean:
	rm -rf dist fn.wasm node_modules

wit-deps: ## Pull the WIT packages guestfn.lock pins into wit/deps
	$(GUESTFN) wit fetch --dir $(CURDIR)
