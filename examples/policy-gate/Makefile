# The policy-gate guest in TypeScript, componentized with jco: needs node and
# npm (protoc only to regenerate the checked-in codec). Render needs the
# crossplane CLI and Docker (the composition's first step,
# function-go-templating, runs in a container), render-check also xprin.
.PHONY: build test render render-check gen-proto clean

node_modules: package-lock.json
	npm ci --no-audit --no-fund

build: node_modules ## Bundle and componentize the guest to fn.wasm
	npm run build

test: node_modules ## Type-check, then run the unit tests natively under node
	npm test

gen-proto: node_modules ## Regenerate the checked-in protobuf-es codec (protoc)
	npm run gen-proto

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf dist fn.wasm node_modules
