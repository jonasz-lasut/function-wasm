# The hello guest as an ABI v2 component: needs a Rust toolchain with the
# wasm32-wasip2 target (rustup target add wasm32-wasip2) and protoc for
# prost-build. Render needs the crossplane CLI and Docker.
.PHONY: build test render render-check clean

build: ## Compile the guest to fn.wasm (a component, built on stable Rust)
	cargo build --release --target wasm32-wasip2
	cp target/wasm32-wasip2/release/hello_rust_v2.wasm fn.wasm

test: ## Run the unit tests natively
	cargo test

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	cargo clean
	rm -f fn.wasm
