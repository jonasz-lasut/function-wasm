# The hello guest in C: needs zig (0.16) on PATH - zig cc compiles the C
# sources and build.zig fetches nanopb and cJSON; nanopb_generator
# (pip install nanopb) is only needed to regenerate the vendored fnv1 codec
# (make gen-proto). Render needs the crossplane CLI and Docker.
.PHONY: build test gen-proto render render-check clean

build: ## Compile the guest to fn.wasm (a wasip1 reactor)
	zig build -Doptimize=ReleaseSmall
	cp zig-out/bin/fn.wasm fn.wasm

test: ## Run the unit tests natively
	zig build test

gen-proto: ## Regenerate src/fnv1 from proto/ (needs nanopb_generator)
	zig build gen-proto

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf .zig-cache zig-out zig-pkg zig-prefix fn.wasm
