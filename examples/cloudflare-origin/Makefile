# The cloudflare-origin guest, an ABI v2 component: needs Rust 1.100 or
# newer with the wasm32-wasip3 target - rust-toolchain.toml pins 1.100's beta
# until that release, and rustup installs it on first use - and protoc for
# prost-build. Render needs the crossplane CLI and Docker, render-check also
# xprin.
.PHONY: build lint test render render-check clean

# The one place this guest's wasm target is named; CI's lint step runs
# `make lint` so the target travels with the guest, not the workflow.
TARGET := wasm32-wasip3

build: ## Compile the guest to fn.wasm (a component)
	cargo build --release --target $(TARGET)
	cp target/$(TARGET)/release/cloudflare_origin.wasm fn.wasm

lint: ## Check formatting, run clippy natively and for the wasm target
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo clippy --target $(TARGET) -- -D warnings

test: ## Run the unit tests natively
	cargo test

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	cargo clean
	rm -f fn.wasm
