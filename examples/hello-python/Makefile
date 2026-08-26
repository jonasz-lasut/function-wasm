# The hello guest in Python, componentized with componentize-py: needs
# python3 (protoc only to regenerate the checked-in codec). Render needs
# the crossplane CLI and Docker.
.PHONY: build test render render-check gen-proto clean

.venv: requirements.txt
	python3 -m venv .venv
	.venv/bin/pip install --quiet -r requirements.txt
	touch .venv

build: .venv ## Componentize the guest to fn.wasm
	VIRTUAL_ENV=$(CURDIR)/.venv .venv/bin/componentize-py -d wit -w function componentize app -p src -p src/gen -o fn.wasm

test: .venv ## Run the unit tests natively under python
	.venv/bin/python -m unittest discover -s test

gen-proto: ## Regenerate the checked-in protobuf codec (protoc)
	protoc --python_out=src/gen -I proto run_function.proto

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf .venv fn.wasm src/__pycache__ src/gen/__pycache__ test/__pycache__
