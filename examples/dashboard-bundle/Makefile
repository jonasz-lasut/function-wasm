# The dashboard-bundle guest in C#, an ABI v2 component built with
# componentize-dotnet on NativeAOT-LLVM: needs the .NET 10 SDK (protoc only
# to regenerate the checked-in codec, python3 only to repack the fixture
# bundles). NativeAOT-LLVM publishes its compiler for linux-x64, linux-arm64
# and win-x64 hosts only, so a Linux host with dotnet builds natively and
# every other host (macOS) runs dotnet in the .NET SDK container - `make
# build DOTNET=dotnet` forces the host's own. Render needs the crossplane CLI
# and Docker, render-check also xprin.
.PHONY: build lint test render render-check gen-proto bundles clean

SDK_IMAGE := mcr.microsoft.com/dotnet/sdk:10.0
# A named volume keeps the container's NuGet packages and WASI SDK between
# builds.
CACHE_VOLUME := function-wasm-dotnet

ifeq ($(shell uname -s)-$(shell command -v dotnet >/dev/null 2>&1 && echo dotnet),Linux-dotnet)
DOTNET := dotnet
else
DOTNET := docker run --rm -v "$(CURDIR):/src" -v $(CACHE_VOLUME):/root -w /src \
	-e DOTNET_CLI_TELEMETRY_OPTOUT=1 -e DOTNET_NOLOGO=1 $(SDK_IMAGE) dotnet
endif

build: ## Compile the guest to fn.wasm (a component)
	$(DOTNET) build -c Release DashboardBundle.csproj
	cp bin/Release/net10.0/wasi-wasm/publish/dashboard-bundle.wasm fn.wasm

lint: ## Check formatting and code style (dotnet format), protoc's codec left out
	$(DOTNET) format DashboardBundle.csproj --verify-no-changes --exclude src/Gen
	$(DOTNET) format test/DashboardBundle.Tests.csproj --verify-no-changes --exclude src/Gen

test: ## Run the unit tests (plain .NET, src/App.cs left out)
	$(DOTNET) test test

gen-proto: ## Regenerate the checked-in protobuf codec (protoc)
	protoc --csharp_out=src/Gen -I proto run_function.proto

bundles: ## Repack example/fixtures/bundles from example/bundles and print the digests to pin
	python3 example/bundles/pack.py

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf bin obj test/bin test/obj fn.wasm
