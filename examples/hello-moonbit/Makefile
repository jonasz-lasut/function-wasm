# The hello guest in MoonBit on ABI v2: needs moon (the MoonBit toolchain,
# https://www.moonbitlang.com/download) on PATH and guestfn, which embeds the
# world (UTF-16) into the core module moon links and wraps it into the
# component the runtime loads (no wasm-tools, no adapter). wit-bindgen 0.62.0
# is only needed to regenerate the checked-in bindings (make gen-bindings)
# and protoc with protoc-gen-mbt 0.2.0 (run through moonx, no install) the
# checked-in codec (make gen-proto). Render needs the crossplane CLI and
# Docker.
.PHONY: build test lint gen-proto gen-bindings render render-check clean

GUESTFN ?= guestfn
MODULE := jonasz-lasut/hello-moonbit
PROTOC_GEN_MBT_VERSION := 0.2.0
WIT_BINDGEN := wit-bindgen

build: ## Compile the guest to fn.wasm (a component)
	$(GUESTFN) build --dir $(CURDIR)

test: ## Run the unit tests (moon test, on moonrun)
	moon test --target wasm -p $(MODULE)/hello

# moon fmt migrates every manifest it touches to the current format and
# reformats generated code too, so the check covers the hand-written
# packages; the whole module must still type-check.
lint: ## Check formatting of the hand-written packages and type-check the module
	moon fmt --check src/hello src/wasmfn
	moon check --target wasm

# protoc resolves the google/protobuf imports from its own include directory;
# protoc-gen-mbt writes the google types nowhere (the moonbitlang/protobuf
# runtime ships them) and the message package under its own module layout,
# whose one file is the codec.
gen-proto: ## Regenerate src/fnv1 from proto/ (needs protoc; protoc-gen-mbt runs through moonx)
	mkdir -p _build/codec
	printf '#!/bin/sh\nexec moonx moonbitlang/protoc-gen-mbt@$(PROTOC_GEN_MBT_VERSION) "$$@"\n' > _build/codec/protoc-gen-mbt
	chmod +x _build/codec/protoc-gen-mbt
	rm -rf _build/codec/out && mkdir -p _build/codec/out
	protoc --plugin=protoc-gen-mbt=_build/codec/protoc-gen-mbt --mbt_out=_build/codec/out \
		--mbt_opt=project_name=fnv1,async=false,json=false,derive=Eq -I proto proto/run_function.proto
	cp _build/codec/out/fnv1/src/apiextensions/fn/proto/v1/top.mbt src/fnv1/top.mbt

# The export package's config is generated too, and run.mbt (hand-written,
# the run slot beside the bindings) needs the glue imported there, so it is
# written back after the generator; and the generator writes each file's
# FFI helpers in an order that changes from run to run, so sort-ffi.pl puts
# them in one (the drift check in e2e.yml compares the result byte for byte).
gen-bindings: ## Regenerate the wit-bindgen output under src/ from wit/ (needs wit-bindgen 0.62.0)
	$(WIT_BINDGEN) moonbit wit --world function --out-dir src --ignore-module-file \
		--project-name $(MODULE) --derive-show --derive-eq
	printf '{\n  "warn-list": "-44",\n  "import": [\n    { "path": "$(MODULE)/wasmfn", "alias": "wasmfn" }\n  ]\n}\n' \
		> src/gen/world/function/moon.pkg.json
	find src -name 'ffi*.mbt' -exec perl sort-ffi.pl {} +

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf _build .mooncakes fn.wasm
