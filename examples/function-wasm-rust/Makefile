# The function-wasm-rust guest, an ABI v2 component: needs Rust 1.100 or
# newer with the wasm32-wasip3 target - rust-toolchain.toml names stable with
# the target, which rustup adds on first use - and protoc for prost-build. Render needs the crossplane CLI and Docker.
.PHONY: build lint test render clean wit-deps

ROOT := $(abspath ../..)
GUESTFN ?= cargo run --manifest-path $(ROOT)/Cargo.toml --release -p guestfn --

# The one place this guest's wasm target is named; CI's lint step runs
# `make lint` so the target travels with the guest, not the workflow.
TARGET := wasm32-wasip3

build: wit-deps ## Compile the guest to fn.wasm (a component)
	cargo build --release --target $(TARGET)
	cp target/$(TARGET)/release/function_wasm_rust.wasm fn.wasm

lint: wit-deps ## Check formatting, run clippy natively and for the wasm target
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo clippy --target $(TARGET) -- -D warnings

test: ## Run the unit tests natively
	cargo test

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)


clean:
	cargo clean
	rm -f fn.wasm

wit-deps: ## Pull the WIT packages guestfn.lock pins into wit/deps
	$(GUESTFN) wit fetch --dir $(CURDIR)
