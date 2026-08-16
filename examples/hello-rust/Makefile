# The hello guest in Rust: needs a Rust toolchain with the wasm32-wasip1
# target (rustup target add wasm32-wasip1) and protoc for prost-build. Render
# needs the crossplane CLI and Docker.
.PHONY: build test render render-check clean

build: ## Compile the guest to fn.wasm (a wasip1 reactor built as a cdylib)
	cargo build --release --target wasm32-wasip1
	cp target/wasm32-wasip1/release/hello_rust.wasm fn.wasm

test: ## Run the unit tests natively
	cargo test

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	cargo clean
	rm -f fn.wasm
