# The hello guest built with TinyGo. `generate` needs protoc (the plugins are
# built from go.mod's tool directives by go generate itself); render needs the
# crossplane CLI and Docker.
.PHONY: generate build test render render-check clean

generate: ## Regenerate internal/fnv1 from proto/run_function.proto
	go generate ./...

build: ## Compile the guest to fn.wasm with TinyGo (a wasip1 reactor)
	tinygo build -target=wasip1 -buildmode=c-shared -no-debug -o fn.wasm .

test: ## Run the unit tests natively with the Go toolchain
	go test ./...

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf fn.wasm .bin
