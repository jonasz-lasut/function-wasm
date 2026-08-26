# The hello guest in AssemblyScript: needs node + npm on PATH (npm ci installs
# assemblyscript, as-proto and json-as locally). protoc and the as-proto-gen
# plugin are only needed to regenerate the vendored fnv1 codec (make
# gen-proto). Render needs the crossplane CLI and Docker.
.PHONY: build test gen-proto render render-check clean

build: node_modules ## Compile the guest to fn.wasm (asc, release target)
	npm run build

node_modules: package-lock.json
	npm ci --no-audit --no-fund

# The behaviour tests run through the real host: TestRunFunctionGuests in the
# repository root builds this guest and drives it through function-wasm, and
# make render-check renders it. There is no separate native test harness.
test: build

# Four files stay hand-written and are kept out of protoc's way: Value.ts
# (as-proto-gen writes every member of the kind oneof, and a Value must
# remember which member is set to copy request state back unchanged),
# Result.ts/Condition.ts (proto3 optional fields need explicit presence) and
# RequestMeta.ts (packed repeated enums, which the generator cannot decode).
gen-proto: node_modules ## Regenerate assembly/fnv1 from proto/ (needs protoc)
	rm -rf .gen && mkdir .gen
	protoc --plugin=protoc-gen-as=./node_modules/.bin/as-proto-gen --as_out=.gen \
		-I proto proto/run_function.proto proto/google/protobuf/struct.proto proto/google/protobuf/duration.proto
	rm .gen/google/protobuf/Value.ts \
		.gen/apiextensions/fn/proto/v1/Result.ts \
		.gen/apiextensions/fn/proto/v1/Condition.ts \
		.gen/apiextensions/fn/proto/v1/RequestMeta.ts
	cp -R .gen/. assembly/fnv1/
	rm -rf .gen

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf node_modules .gen fn.wasm
