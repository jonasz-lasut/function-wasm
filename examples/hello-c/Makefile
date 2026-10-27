# The hello guest in C on ABI v2: needs zig (0.16) on PATH - zig cc compiles
# the C sources and build.zig fetches nanopb - and guestfn, which comes from
# this repository (cargo run) and wraps the core module zig links into the
# component the runtime loads (no wasm-tools, no adapter download).
# wit-bindgen 0.62.0 is only needed to regenerate the checked-in bindings
# (make gen-bindings; the root Makefile's gen-bindings installs it pinned and
# mirrors the result into the c template) and nanopb_generator (pip install
# nanopb) the checked-in codec (make gen-proto). Render needs the crossplane
# CLI and Docker. GUESTFN and FUNCTION_BIN (read by ../render.sh) can name
# prebuilt binaries instead.
ROOT := $(abspath ../..)
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --

.PHONY: build test gen-proto gen-bindings render clean

build: ## Compile the guest to fn.wasm (a component)
	$(GUESTFN) build --dir $(CURDIR)

test: ## Run the unit tests natively
	zig build test

gen-proto: ## Regenerate src/fnv1 from proto/ (needs nanopb_generator)
	zig build gen-proto

gen-bindings: ## Regenerate src/gen from wit/ (needs wit-bindgen 0.62.0)
	zig build gen-bindings

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)


clean:
	rm -rf .zig-cache zig-out zig-pkg zig-prefix fn.wasm
