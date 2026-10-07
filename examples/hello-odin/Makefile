# The hello guest in Odin on ABI v2: needs odin (dev-2026-10) and zig (0.16)
# on PATH - odin compiles the guest to a wasm32 object, zig cc compiles the C
# half (build.zig fetches nanopb) and links the core module - and guestfn,
# which comes from this repository (cargo run) and wraps that core module into
# the component the runtime loads (no wasm-tools, no adapter download).
# wit-bindgen 0.62.0 is only needed to regenerate the checked-in bindings
# (make gen-bindings; the root Makefile's gen-bindings installs it pinned)
# and nanopb_generator (pip install nanopb) the checked-in codec (make
# gen-proto). Render needs the crossplane CLI and Docker, render-check also
# xprin. GUESTFN, FUNCTION_BIN (read by ../render.sh) and XPRIN can name
# prebuilt binaries instead, as CI does.
ROOT := $(abspath ../..)
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --

.PHONY: build test lint gen-proto gen-bindings render render-check clean

build: ## Compile the guest to fn.wasm (a component)
	$(GUESTFN) build --dir $(CURDIR)

test: ## Run the unit tests natively
	zig build test

lint: ## Vet the Odin sources for the wasm target and check build.zig's formatting
	zig fmt --check build.zig
	zig build lint

gen-proto: ## Regenerate src/fnv1 from proto/ (needs nanopb_generator)
	zig build gen-proto

gen-bindings: ## Regenerate src/gen from wit/ (needs wit-bindgen 0.62.0)
	zig build gen-bindings

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf .zig-cache zig-out zig-pkg zig-prefix fn.wasm
