# The hello guest in C#, an ABI v2 component built with componentize-dotnet
# on NativeAOT-LLVM: needs the .NET 10 SDK (protoc only to regenerate the
# checked-in codec). NativeAOT-LLVM publishes its compiler for linux-x64,
# linux-arm64 and win-x64 hosts only, so a Linux host with dotnet builds
# natively and every other host (macOS) runs dotnet in the .NET SDK container
# - `make build DOTNET=dotnet` forces the host's own. Render needs the
# crossplane CLI and Docker.
.PHONY: build test render render-check gen-proto clean

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
	$(DOTNET) build -c Release HelloDotnet.csproj
	cp bin/Release/net10.0/wasi-wasm/publish/hello-dotnet.wasm fn.wasm

test: ## Run the unit tests (plain .NET, src/App.cs left out)
	$(DOTNET) test test

gen-proto: ## Regenerate the checked-in protobuf codec (protoc)
	protoc --csharp_out=src/Gen -I proto run_function.proto

render: build ## Start the runtime serving this directory and crossplane render example/
	../render.sh $(CURDIR)

render-check: build ## Like render, but assert the output (used by CI)
	../render.sh $(CURDIR) --check

clean:
	rm -rf bin obj test/bin test/obj fn.wasm
