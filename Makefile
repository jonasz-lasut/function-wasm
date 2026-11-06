# function-wasm's Makefile. Two kinds of targets live here:
#
# - The end-to-end suite's cluster (`make e2e`, "Testing" in AGENTS.md and
#   docs/project/testing.md): a kind cluster with Crossplane, provider-nop,
#   a module registry and this tree's runtime installed as a Function, and
#   the Rust suite under crates/e2e run against it, one profile at a time.
#   The crossplane build submodule (build/) provides the make library:
#   common.mk's pipelines and variables, k8s_tools.mk's pinned kind,
#   kubectl, helm and crossplane CLI (installed under .cache/tools on first
#   use), controlplane.mk's kind + Crossplane cluster at CROSSPLANE_VERSION,
#   and local.xpkg.mk's package cache sync, which installs a locally built
#   package with packagePullPolicy: Never. What the library lacks for a
#   Function package - xpkg.build embeds the runtime image only for a
#   Provider, and there is no local.xpkg.deploy.function.% - is written
#   below, in the library's shape.
#
# - The vendoring loops (vendor-proto, vendor-docs-highlight, gen-bindings):
#   further down, unchanged by the submodule.
PROJECT_NAME := function-wasm
PROJECT_REPO := github.com/jonasz-lasut/$(PROJECT_NAME)
# The runtime image's platforms; the suite builds the host's only.
PLATFORMS ?= linux_amd64 linux_arm64

ifeq (,$(wildcard build/makelib/common.mk))
$(error the crossplane build submodule is missing: run git submodule update --init)
endif

# The cluster the suite provisions and the Crossplane it installs. Both are
# overridable from the environment: /e2e runs the default, the release
# matrix (.github/workflows/release-matrix-e2e.yml) runs the suite once per
# Crossplane minor the project supports, setting CROSSPLANE_VERSION for the
# two older ones. Renovate moves the default to the latest release (the
# `# renovate:` comment); its PR for a new minor is the signal to shift the
# matrix's two pins down one minor in the same PR.
KIND_CLUSTER_NAME ?= function-wasm-e2e
# renovate: datasource=github-releases depName=crossplane/crossplane
CROSSPLANE_VERSION ?= 2.4.3
PROVIDER_NOP_VERSION ?= v0.6.0

include build/makelib/common.mk
include build/makelib/k8s_tools.mk
include build/makelib/controlplane.mk
# local.xpkg.mk reads the package from XPKG_OUTPUT_DIR/linux_<arch>/, where
# xpkg.mk would have written it; the Function package target below does.
XPKG_OUTPUT_DIR := $(OUTPUT_DIR)/xpkg
include build/makelib/local.xpkg.mk

# The end-to-end suite
#
# make e2e            up, the three profiles in turn, down (the library's
#                     e2e pipeline: e2e.init, e2e.run, e2e.done)
# make e2e.up         the cluster: kind + Crossplane (controlplane.up, then
#                     e2e.crossplane waits for its deployment and the package
#                     CRDs its init container registers), the
#                     suite's setup (crates/e2e's e2e-setup binary: the
#                     registry with its TLS and auth, provider-nop, the test
#                     module pushed and signed under every variant, the
#                     profiles' ConfigMaps and Secrets, the XRD), the runtime
#                     image and the Function package installed from it
# make e2e.test.<p>   one profile's suite: cargo test --test <p>, which
#                     switches the Function to that profile's runtime config
#                     first (default, strict, lifecycle)
# make e2e.test       the three in turn
# make e2e.down       delete the cluster
# make e2e.guest      rebuild the committed test module, crates/e2e/guest/fn.wasm
# make print-crossplane-version
#                     the CROSSPLANE_VERSION in effect (the release matrix
#                     reads the default this way)
#
# FUNCTION_BIN names a prebuilt linux runtime binary for the host's
# architecture (CI's build-tools job hands one over), wrapped by
# Dockerfile.dev; without it the image is built from the Dockerfile, which
# compiles the workspace in a container.
E2E_IMAGE := function-wasm-e2e:dev
E2E_DIR := $(OUTPUT_DIR)/e2e
E2E_PROFILES := default strict lifecycle
E2E_CARGO := cargo run -q -p function-wasm-e2e --features cluster
E2E_GUEST_DIR := crates/e2e/guest
FUNCTION_PKG_EXAMPLES := examples/pdb-addon/example
# The image and the DeploymentRuntimeConfig local.xpkg.deploy.function.%
# installs a Function with; the oci targets below set theirs.
FUNCTION_IMAGE := $(E2E_IMAGE)
FUNCTION_RUNTIME_CONFIG := e2e-default

# The OCI runtime (docs/one-pager-oci-runtime.md, docs/operators/oci-runtime.md):
# the VM pieces the engine boots from - the Firecracker release and the
# guest kernel Firecracker's CI publishes, pinned here as in Dockerfile.oci
# - the init and the fixture built static for musl, the engine's suite over
# a real VM (make oci.test, under KVM), and the end-to-end suite's oci
# profile: the function-oci package beside function-wasm's cluster
# (make e2e.oci.up, then make e2e.test.oci). FUNCTION_OCI_TOOLS names a
# directory of prebuilt function, init, firecracker and vmlinux (the OCI
# workflow's build job hands one over); without it they are built here.
FIRECRACKER_VERSION ?= v1.17.0
GUEST_KERNEL_CI ?= v1.15
GUEST_KERNEL_VERSION ?= 6.1.155
OCI_ARCH := $(if $(filter arm64 aarch64,$(shell uname -m)),aarch64,x86_64)
# The checksums Dockerfile.oci pins, per architecture.
FIRECRACKER_SHA256_x86_64 := 06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558
FIRECRACKER_SHA256_aarch64 := e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256
GUEST_KERNEL_SHA256_x86_64 := e20e46d0c36c55c0d1014eb20576171b3f3d922260d9f792017aeff53af3d4f2
GUEST_KERNEL_SHA256_aarch64 := e3544b10603acbf3db492cb52e000d22ba202cb4b63b9add027565683e11c591
MUSL_TARGET := $(OCI_ARCH)-unknown-linux-musl
OCI_TOOLS := .cache/tools/oci
E2E_OCI_IMAGE := function-oci-e2e:dev
E2E_OCI_GUEST ?= $(E2E_GUEST_DIR)/target/$(MUSL_TARGET)/release/e2e-guest-oci

.PHONY: e2e.up e2e.crossplane e2e.setup e2e.image e2e.test e2e.down e2e.guest e2e.dump xpkg.build.function-wasm print-crossplane-version
.PHONY: oci.tools oci.init oci.test e2e.oci.guest e2e.oci.image e2e.oci.up e2e.test.oci xpkg.build.function-oci
print-crossplane-version:
	@echo $(CROSSPLANE_VERSION)

e2e.init: e2e.up
e2e.run: e2e.test
e2e.done: e2e.down

e2e.up: controlplane.up e2e.crossplane e2e.setup xpkg.build.function-wasm local.xpkg.deploy.function.function-wasm
	@$(INFO) waiting for the function to become healthy
	@$(KUBECTL) wait function.pkg.crossplane.io/function-wasm --for=condition=Healthy --timeout=300s || $(FAIL)
	@$(OK) the end-to-end cluster is up, run make e2e.test.default, e2e.test.strict or e2e.test.lifecycle

# controlplane.up returns as soon as the chart is installed; Crossplane's
# init container registers the package CRDs after that, and an older
# release takes long enough for the setup's Provider to meet a 404.
e2e.crossplane: $(KUBECTL)
	@$(INFO) waiting for crossplane and its package api
	@$(KUBECTL) wait --namespace $(CROSSPLANE_NAMESPACE) --for=condition=Available --timeout=300s deployment/crossplane || $(FAIL)
	@$(KUBECTL) wait --for=condition=Established --timeout=120s crd/providers.pkg.crossplane.io crd/functions.pkg.crossplane.io crd/deploymentruntimeconfigs.pkg.crossplane.io || $(FAIL)
	@$(OK) waiting for crossplane and its package api

# E2E_OCI_GUEST, when the file exists, adds the oci profile's image and
# variants to what setup pushes (the oci targets build it first).
e2e.setup:
	@$(INFO) setting up the registry, provider-nop and the test module
	@PROVIDER_NOP_VERSION=$(PROVIDER_NOP_VERSION) $(if $(wildcard $(E2E_OCI_GUEST)),E2E_OCI_GUEST=$(E2E_OCI_GUEST),) $(E2E_CARGO) --bin e2e-setup || $(FAIL)
	@$(OK) setting up the registry, provider-nop and the test module

e2e.image:
	@$(INFO) building the runtime image $(E2E_IMAGE) for linux/$(TARGETARCH)
ifdef FUNCTION_BIN
	@mkdir -p $(E2E_DIR)/prebuilt && cp $(FUNCTION_BIN) $(E2E_DIR)/prebuilt/function
	@docker buildx build --load --platform linux/$(TARGETARCH) -f Dockerfile.dev -t $(E2E_IMAGE) $(E2E_DIR)/prebuilt || $(FAIL)
else
	@docker buildx build --load --platform linux/$(TARGETARCH) --target image -t $(E2E_IMAGE) . || $(FAIL)
endif
	@$(OK) building the runtime image $(E2E_IMAGE)

# The Function package from the image: what xpkg.mk's xpkg.build.% would do
# for a Provider. --examples-root must hold Crossplane objects only (xpkg
# build parses every YAML file under it), which the pdb-addon example's
# directory does.
xpkg.build.function-wasm: $(CROSSPLANE_CLI) e2e.image
	@$(INFO) building package function-wasm-$(VERSION).xpkg
	@mkdir -p $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)
	@$(CROSSPLANE_CLI) xpkg build --package-root package --examples-root $(FUNCTION_PKG_EXAMPLES) \
		--embed-runtime-image $(E2E_IMAGE) \
		--package-file $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)/function-wasm-$(VERSION).xpkg || $(FAIL)
	@$(OK) building package function-wasm-$(VERSION).xpkg

# local.xpkg.deploy.provider.%'s shape for a Function: the runtime image
# loaded into the kind node, the profiles' DeploymentRuntimeConfigs applied,
# the Function installed from the synced cache with packagePullPolicy Never
# on the default profile.
local.xpkg.deploy.function.%: $(KIND) $(KUBECTL) local.xpkg.sync
	@$(INFO) deploying function package $* $(VERSION)
	@$(KIND) load docker-image $(FUNCTION_IMAGE) -n $(KIND_CLUSTER_NAME) || $(FAIL)
	@for p in $(E2E_PROFILES) oci; do $(KUBECTL) apply -f crates/e2e/profiles/$$p/drc.yaml || exit 1; done
	@echo '{"apiVersion":"pkg.crossplane.io/v1","kind":"Function","metadata":{"name":"$*"},"spec":{"package":"xpkg.crossplane.internal/dev/$*@$(LOCAL_XPKG_DIGEST)","skipDependencyResolution":true,"packagePullPolicy":"Never","runtimeConfigRef":{"name":"$(FUNCTION_RUNTIME_CONFIG)"}}}' | $(KUBECTL) apply -f - || $(FAIL)
	@$(OK) deploying function package $* $(VERSION)

# The OCI runtime's targets.
oci.tools: $(OCI_TOOLS)/firecracker $(OCI_TOOLS)/vmlinux ## Fetch the pinned Firecracker release and guest kernel under .cache/tools/oci

# The release tarball is unpacked beside the binary and left there, a cache.
$(OCI_TOOLS)/firecracker:
	@echo "fetching firecracker $(FIRECRACKER_VERSION) ($(OCI_ARCH))"
	@mkdir -p $(OCI_TOOLS)/release
	@curl -fsSL -o $(OCI_TOOLS)/release/firecracker.tgz https://github.com/firecracker-microvm/firecracker/releases/download/$(FIRECRACKER_VERSION)/firecracker-$(FIRECRACKER_VERSION)-$(OCI_ARCH).tgz
	@echo "$(FIRECRACKER_SHA256_$(OCI_ARCH))  $(OCI_TOOLS)/release/firecracker.tgz" | sha256sum -c - >/dev/null
	@tar -xzf $(OCI_TOOLS)/release/firecracker.tgz -C $(OCI_TOOLS)/release
	@cp $(OCI_TOOLS)/release/release-$(FIRECRACKER_VERSION)-$(OCI_ARCH)/firecracker-$(FIRECRACKER_VERSION)-$(OCI_ARCH) $@
	@chmod 0755 $@

$(OCI_TOOLS)/vmlinux:
	@echo "fetching the guest kernel $(GUEST_KERNEL_VERSION) ($(GUEST_KERNEL_CI), $(OCI_ARCH))"
	@mkdir -p $(OCI_TOOLS)
	@curl -fsSL -o $@ https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/$(GUEST_KERNEL_CI)/$(OCI_ARCH)/vmlinux-$(GUEST_KERNEL_VERSION)
	@echo "$(GUEST_KERNEL_SHA256_$(OCI_ARCH))  $@" | sha256sum -c - >/dev/null

oci.init: ## Build the guest init and the test fixture (the engine crate's binaries) static for musl
	@rustup target add $(MUSL_TARGET) >/dev/null
	@cargo build --release -p function-wasm-engine --no-default-features --bins --target $(MUSL_TARGET) || $(FAIL)

oci.test: oci.tools oci.init ## The OCI engine's suite over a real Firecracker VM (needs /dev/kvm)
	@GUESTFN_FIRECRACKER=$(CURDIR)/$(OCI_TOOLS)/firecracker GUESTFN_KERNEL=$(CURDIR)/$(OCI_TOOLS)/vmlinux \
	 GUESTFN_INIT=$(CURDIR)/target/$(MUSL_TARGET)/release/guestfn-init \
	 GUESTFN_FIXTURE=$(CURDIR)/target/$(MUSL_TARGET)/release/guestfn-guest-fixture GUESTFN_VM_CONSOLE=1 \
	 cargo test -p function-wasm-engine --no-default-features --features oci -- --nocapture || $(FAIL)

# The test module as a process, static for musl, on the guest's own pinned
# toolchain (its rust-toolchain.toml).
e2e.oci.guest:
	@$(INFO) building $(E2E_OCI_GUEST)
	@cd $(E2E_GUEST_DIR) && rustup target add $(MUSL_TARGET) >/dev/null && cargo build --release --features process --bin e2e-guest-oci --target $(MUSL_TARGET) || $(FAIL)
	@$(OK) building $(E2E_OCI_GUEST)

# The function-oci image: the four files from FUNCTION_OCI_TOOLS, or built
# and fetched here, wrapped by Dockerfile.oci.dev.
e2e.oci.image:
	@$(INFO) building the runtime image $(E2E_OCI_IMAGE) for linux/$(TARGETARCH)
	@mkdir -p $(E2E_DIR)/prebuilt-oci
ifdef FUNCTION_OCI_TOOLS
	@cp $(FUNCTION_OCI_TOOLS)/function $(FUNCTION_OCI_TOOLS)/init $(FUNCTION_OCI_TOOLS)/firecracker $(FUNCTION_OCI_TOOLS)/vmlinux $(E2E_DIR)/prebuilt-oci/
else
	@$(MAKE) oci.tools oci.init
	@cargo build --release -p function-wasm --no-default-features --features oci || $(FAIL)
	@cp target/release/function $(E2E_DIR)/prebuilt-oci/function
	@cp target/$(MUSL_TARGET)/release/guestfn-init $(E2E_DIR)/prebuilt-oci/init
	@cp $(OCI_TOOLS)/firecracker $(OCI_TOOLS)/vmlinux $(E2E_DIR)/prebuilt-oci/
endif
	@docker buildx build --load --platform linux/$(TARGETARCH) -f Dockerfile.oci.dev -t $(E2E_OCI_IMAGE) $(E2E_DIR)/prebuilt-oci || $(FAIL)
	@$(OK) building the runtime image $(E2E_OCI_IMAGE)

xpkg.build.function-oci: $(CROSSPLANE_CLI) e2e.oci.image
	@$(INFO) building package function-oci-$(VERSION).xpkg
	@mkdir -p $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)
	@$(CROSSPLANE_CLI) xpkg build --package-root package/oci --examples-root $(FUNCTION_PKG_EXAMPLES) \
		--embed-runtime-image $(E2E_OCI_IMAGE) \
		--package-file $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)/function-oci-$(VERSION).xpkg || $(FAIL)
	@$(OK) building package function-oci-$(VERSION).xpkg

local.xpkg.deploy.function.function-oci: FUNCTION_IMAGE := $(E2E_OCI_IMAGE)
local.xpkg.deploy.function.function-oci: FUNCTION_RUNTIME_CONFIG := e2e-oci

# The oci profile's cluster: kind and Crossplane, the setup with the module
# as a process pushed too, and function-oci installed under the e2e-oci
# config (/dev/kvm from the node). make e2e.up adds function-wasm beside it.
# The guest as a process is built unless E2E_OCI_GUEST already names one
# (the OCI workflow's cluster job gets it from the build job).
e2e.oci.up: $(if $(wildcard $(E2E_OCI_GUEST)),,e2e.oci.guest) controlplane.up e2e.crossplane e2e.setup xpkg.build.function-oci local.xpkg.deploy.function.function-oci
	@$(INFO) waiting for function-oci to become healthy
	@$(KUBECTL) wait function.pkg.crossplane.io/function-oci --for=condition=Healthy --timeout=300s || $(FAIL)
	@$(OK) the oci profile cluster is up, run make e2e.test.oci

e2e.test.oci:
	@$(INFO) running the oci profile suite
	@cargo test -p function-wasm-e2e --features cluster --test oci || $(FAIL)
	@$(OK) running the oci profile suite

e2e.test: $(addprefix e2e.test.,$(E2E_PROFILES))

# One explicit target per profile (a phony target is never matched by a
# pattern rule).
define e2e.test.profile
.PHONY: e2e.test.$(1)
e2e.test.$(1):
	@$$(INFO) running the $(1) profile suite
	@cargo test -p function-wasm-e2e --features cluster --test $(1) || $$(FAIL)
	@$$(OK) running the $(1) profile suite
endef
$(foreach p,$(E2E_PROFILES),$(eval $(call e2e.test.profile,$(p))))

e2e.down: controlplane.down

# The cluster's state for a failed run (controlplane.dump plus the suite's
# own logs under _output/e2e/logs); CI uploads the directory.
e2e.dump: controlplane.dump

# The committed test module. cargo builds it on the rust scaffold's pinned
# toolchain (crates/e2e/guest/rust-toolchain.toml); guestfn inspect states
# the verdict the runtime will give it.
e2e.guest:
	@$(INFO) building $(E2E_GUEST_DIR)/fn.wasm
	@cd $(E2E_GUEST_DIR) && cargo build --release --target wasm32-wasip3 || $(FAIL)
	@cp $(E2E_GUEST_DIR)/target/wasm32-wasip3/release/e2e_guest.wasm $(E2E_GUEST_DIR)/fn.wasm
	@cargo run -q -p guestfn -- inspect $(E2E_GUEST_DIR)/fn.wasm || $(FAIL)
	@$(OK) building $(E2E_GUEST_DIR)/fn.wasm

# The vendoring loops
#
# The vendoring loops
#
# Re-vendoring crossplane/crossplane's proto/fn/v1/run_function.proto (AGENTS.md
# "Changing the scaffold"). Every tracked proto/run_function.proto - the files
# Renovate's version-header rule reads: five templates, five goldens, seven
# examples - is the upstream file byte for byte under a five-line header whose
# first line names the release. A Renovate bump moves that version in every
# header but can neither re-download the file nor regenerate the codecs checked
# in beside it, so its PR is the signal to run `make vendor-proto` on the PR's
# branch, review the diff and comment /e2e.
#
# `make vendor-proto` takes the release from the headers (the rust template's);
# `make vendor-proto PROTO_VERSION=vX.Y.Z` vendors one Renovate has not proposed.
#
# `make gen-bindings` is the same loop for the wit-bindgen bindings: the c,
# zig and odin guests' C bindings (examples/hello-c/src/gen,
# examples/hello-zig/src/gen and examples/hello-odin/src/gen: one generator's
# output over one world, which the guestfn tests hold identical), the go
# guest's Go bindings (examples/pdb-addon/internal/bindings, over its own
# world) and the moonbit example's MoonBit bindings (examples/hello-moonbit/src,
# over the c world again; no template). It runs the pinned wit-bindgen in
# each, mirrors each scaffold's result into its template (the go one with the
# module path templated as [[ .Module ]]) and refreshes the goldens - after a
# change to a world, a re-vendoring of the WASI WIT, or a wit-bindgen bump
# (the generated files' header names the version; e2e.yml's drift checks
# install the same one).
.PHONY: vendor-proto vendor-proto-fetch vendor-proto-codecs vendor-proto-goldens vendor-docs-highlight gen-bindings tools tools-clean vale design-records
# The steps rewrite one another's inputs; never run them side by side.
.NOTPARALLEL:

# Only the three generators the guests invoke bare from PATH are pinned here;
# the rest are pinned where each guest builds them: zig-protobuf (and the
# protoc it downloads) by hello-zig's build.zig.zon, protoc-gen-es by
# policy-gate's package-lock.json, protoc-gen-mbt by the moonx release
# hello-moonbit's Makefile names (moon itself is not pinned here: e2e.yml
# pins the release its render job installs).
#
# protoc stamps its version into the Python codec, which refuses to load on a
# protobuf runtime older than that stamp, so
# PROTOC_VERSION must not pass the runtime the python template's
# requirements.txt pins (team-tags's is the same file). NANOPB_VERSION is
# the nanopb release hello-c's and hello-odin's build.zig.zon compile and
# e2e.yml's codec drift checks install. WIT_BINDGEN_VERSION is the
# wit-bindgen-cli release that wrote hello-c's, hello-zig's, hello-odin's and
# pdb-addon's checked-in bindings and e2e.yml's bindings drift checks install.
PROTOC_VERSION := 35.1
NANOPB_VERSION := 0.4.9.1
WIT_BINDGEN_VERSION := 0.62.0

PROTOC_OS := $(if $(filter Darwin,$(shell uname -s)),osx,linux)
PROTOC_ARCH := $(if $(filter arm64 aarch64,$(shell uname -m)),aarch_64,x86_64)
WIT_BINDGEN_OS := $(if $(filter Darwin,$(shell uname -s)),macos,linux)
WIT_BINDGEN_ARCH := $(if $(filter arm64 aarch64,$(shell uname -m)),aarch64,x86_64)
TOOLS := $(CURDIR)/.cache/tools/$(PROTOC_OS)-$(PROTOC_ARCH)
PROTOC := $(TOOLS)/protoc-$(PROTOC_VERSION)/bin/protoc
NANOPB_GENERATOR := $(TOOLS)/nanopb-$(NANOPB_VERSION)/bin/nanopb_generator
WIT_BINDGEN := $(TOOLS)/wit-bindgen-$(WIT_BINDGEN_VERSION)/bin/wit-bindgen
# Only the three pinned binaries, never the nanopb venv's bin/ (its python3
# would shadow the system one), go in front of PATH.
WITH_TOOLS := PATH="$(TOOLS)/bin:$$PATH"

# Vale, the prose linter over the documentation (.vale.ini names the paths'
# styles and pins the style packages): the release tarball for this OS and
# architecture, checked against the release's checksum file. Renovate moves
# the pin through the `# renovate:` comment. The packages are synced once
# per change of .vale.ini (the stamp file), under the gitignored part of
# .vale/styles.
# renovate: datasource=github-releases depName=errata-ai/vale
VALE_VERSION := 3.26.0
VALE_OS := $(if $(filter Darwin,$(shell uname -s)),macOS,Linux)
VALE_ARCH := $(if $(filter arm64 aarch64,$(shell uname -m)),arm64,64-bit)
VALE := $(TOOLS)/vale-$(VALE_VERSION)/vale
VALE_SYNCED := .vale/styles/.synced
VALE_PATHS := docs README.md SECURITY.md

PROTO_COPIES := $(shell git ls-files '*/proto/run_function.proto')
PROTO_VERSION_FILE := crates/guestfn/templates/rust/proto/run_function.proto
PROTO_VERSION := $(shell sed -n '1s/.*crossplane\/crossplane \(v[0-9.]*\)\..*/\1/p' $(PROTO_VERSION_FILE))
PROTO_URL := https://raw.githubusercontent.com/crossplane/crossplane/$(PROTO_VERSION)/proto/fn/v1/run_function.proto

vendor-proto: vendor-proto-fetch vendor-proto-codecs vendor-proto-goldens ## Re-vendor every copy at $(PROTO_VERSION), regenerate the codecs and the scaffold goldens
	@echo "vendored run_function.proto $(PROTO_VERSION): review git diff, then comment /e2e on the PR"

# Renovate's header regex reads vX.Y.Z only; anything else would leave the
# copies untracked.
vendor-proto-fetch: ## Overwrite every copy with the upstream file at $(PROTO_VERSION), keeping each copy's own header
	@expr '$(PROTO_VERSION)' : 'v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$$' >/dev/null || \
		{ echo "PROTO_VERSION='$(PROTO_VERSION)' is not a crossplane/crossplane release (vX.Y.Z)" >&2; exit 1; }
	@set -e; \
	upstream=$$(mktemp); trap 'rm -f "$$upstream"' EXIT; \
	echo "fetching $(PROTO_URL)"; \
	curl -fsSL -o "$$upstream" '$(PROTO_URL)'; \
	for f in $(PROTO_COPIES); do \
		case "$$(head -n 1 "$$f")" in "// Vendored from crossplane/crossplane v"*) ;; \
			*) echo "$$f: line 1 is not the vendored-from header" >&2; exit 1 ;; esac; \
		{ head -n 4 "$$f" | sed '1s#crossplane/crossplane v[0-9.]*\.#crossplane/crossplane $(PROTO_VERSION).#'; \
			echo; cat "$$upstream"; } > "$$f.tmp"; \
		mv "$$f.tmp" "$$f"; \
		echo "  $$f"; \
	done

# rust and rust-v2 have no checked-in codec (build.rs runs prost-build); the
# zig, c, ts and python codecs are mirrored into their guestfn
# templates, which examples_share_the_scaffold_plumbing holds identical to
# the examples (hello-odin regenerates the c codec over its own copy of the
# proto, and the same test holds it to the c template's). The ts and python
# codecs are copied file by file: a gen/ directory may hold a __pycache__ the
# templates must not embed. The moonbit codec has no template: hello-moonbit's
# gen-proto runs the pinned protoc-gen-mbt through moonx, so moon must be on
# PATH.
vendor-proto-codecs: tools ## Regenerate every checked-in guest codec with the pinned generators and mirror them into the templates
	$(WITH_TOOLS) $(MAKE) -C examples/hello-zig gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-c gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-odin gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-moonbit gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/team-tags gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/policy-gate gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/dashboard-bundle gen-proto
	rm -rf crates/guestfn/templates/zig/src/fnv1 && cp -R examples/hello-zig/src/fnv1 crates/guestfn/templates/zig/src/fnv1
	rm -rf crates/guestfn/templates/c/src/fnv1 && cp -R examples/hello-c/src/fnv1 crates/guestfn/templates/c/src/fnv1
	for f in run_function_pb.js run_function_pb.d.ts; do \
		cp "examples/policy-gate/src/gen/$$f" "crates/guestfn/templates/ts/src/gen/$$f"; \
	done
	cp examples/team-tags/src/gen/run_function_pb2.py crates/guestfn/templates/python/src/gen/run_function_pb2.py

vendor-proto-goldens: ## Refresh the guestfn scaffold goldens from the templates
	UPDATE_GOLDENS=1 cargo test -p guestfn

# The documentation site's Cedar highlighting (docs/theme/cedar.js) runs
# cedar-policy/highlightjs-cedar's grammar on a private highlight.js 11 core,
# each vendored under docs/theme as the upstream file beneath a one-line
# header naming the release, which Renovate's version-header rules read. A
# bump PR moves a header only, so it is the signal to run
# `make vendor-docs-highlight` on the PR's branch: both files and their
# licenses are fetched again at the versions the headers name.
HLJS_CORE := docs/theme/highlightjs/core.min.js
HLJS_CEDAR := docs/theme/highlightjs-cedar/hljs-cedar.esm.js
HLJS_VERSION := $(shell sed -n '1s|^// Vendored from @highlightjs/cdn-assets \([0-9.]*\) .*|\1|p' $(HLJS_CORE))
HLJS_CEDAR_VERSION := $(shell sed -n '1s|^// Vendored from cedar-policy/highlightjs-cedar \(v[0-9.]*\) .*|\1|p' $(HLJS_CEDAR))
HLJS_CEDAR_URL := https://raw.githubusercontent.com/cedar-policy/highlightjs-cedar/$(HLJS_CEDAR_VERSION)

vendor-docs-highlight: ## Re-vendor the docs site's highlight.js core and Cedar grammar, with their licenses, at the versions their headers name
	@expr '$(HLJS_VERSION)' : '[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$$' >/dev/null || \
		{ echo "$(HLJS_CORE): line 1 names no @highlightjs/cdn-assets release (X.Y.Z)" >&2; exit 1; }
	@expr '$(HLJS_CEDAR_VERSION)' : 'v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$$' >/dev/null || \
		{ echo "$(HLJS_CEDAR): line 1 names no cedar-policy/highlightjs-cedar release (vX.Y.Z)" >&2; exit 1; }
	@set -e; \
	tarball=$$(mktemp); trap 'rm -f "$$tarball"' EXIT; \
	echo "fetching @highlightjs/cdn-assets $(HLJS_VERSION)"; \
	curl -fsSL -o "$$tarball" 'https://registry.npmjs.org/@highlightjs/cdn-assets/-/cdn-assets-$(HLJS_VERSION).tgz'; \
	{ head -n 1 $(HLJS_CORE); tar -xzOf "$$tarball" package/es/core.min.js; } > $(HLJS_CORE).tmp; \
	mv $(HLJS_CORE).tmp $(HLJS_CORE); \
	tar -xzOf "$$tarball" package/LICENSE > $(dir $(HLJS_CORE))LICENSE; \
	echo "fetching cedar-policy/highlightjs-cedar $(HLJS_CEDAR_VERSION)"; \
	{ head -n 1 $(HLJS_CEDAR); curl -fsSL '$(HLJS_CEDAR_URL)/dist/hljs-cedar.mjs'; } > $(HLJS_CEDAR).tmp; \
	mv $(HLJS_CEDAR).tmp $(HLJS_CEDAR); \
	curl -fsSL -o $(dir $(HLJS_CEDAR))LICENSE '$(HLJS_CEDAR_URL)/LICENSE'; \
	curl -fsSL -o $(dir $(HLJS_CEDAR))NOTICE '$(HLJS_CEDAR_URL)/NOTICE'
	@echo "vendored highlight.js $(HLJS_VERSION) and highlightjs-cedar $(HLJS_CEDAR_VERSION): review git diff, then check the Cedar pages under mdbook serve"

# The bindings are mirrored whole: wit-bindgen writes exactly the three files
# the c and zig templates carry (function.c, function.h,
# function_component_type.o), and for go a wit_bindings.go and an empty.s
# per package plus wit_exports, beside the hand-written slot package
# export_wit_world. A generated Go file names the guest's module path in its
# imports, so it becomes a .go.tmpl with the path as [[ .Module ]]; the rest
# is copied as it is.
GO_MODULE := github.com/jonasz-lasut/function-wasm/examples/pdb-addon
GO_BINDINGS := crates/guestfn/templates/go/internal/bindings
gen-bindings: tools ## Regenerate hello-c's, hello-zig's, hello-odin's, pdb-addon's and hello-moonbit's wit-bindgen bindings with the pinned wit-bindgen, mirror the c, zig and go ones into their templates and refresh the goldens
	$(WITH_TOOLS) $(MAKE) -C examples/hello-c gen-bindings
	$(WITH_TOOLS) $(MAKE) -C examples/hello-zig gen-bindings
	$(WITH_TOOLS) $(MAKE) -C examples/hello-odin gen-bindings
	$(WITH_TOOLS) $(MAKE) -C examples/pdb-addon gen-bindings
	$(WITH_TOOLS) $(MAKE) -C examples/hello-moonbit gen-bindings
	rm -rf crates/guestfn/templates/c/src/gen && cp -R examples/hello-c/src/gen crates/guestfn/templates/c/src/gen
	rm -rf crates/guestfn/templates/zig/src/gen && cp -R examples/hello-zig/src/gen crates/guestfn/templates/zig/src/gen
	rm -rf $(GO_BINDINGS) && cp -R examples/pdb-addon/internal/bindings $(GO_BINDINGS)
	@set -e; \
	for f in $$(find $(GO_BINDINGS) -name '*.go' -not -path '*/export_wit_world/*'); do \
		sed 's#$(GO_MODULE)#[[ .Module ]]#g' "$$f" > "$$f.tmpl"; \
		rm "$$f"; \
	done
	$(MAKE) vendor-proto-goldens

tools: $(PROTOC) $(NANOPB_GENERATOR) $(WIT_BINDGEN) ## Install the pinned protoc, nanopb_generator and wit-bindgen under .cache/tools
	@mkdir -p "$(TOOLS)/bin"
	@ln -sf "$(PROTOC)" "$(TOOLS)/bin/protoc"
	@ln -sf "$(NANOPB_GENERATOR)" "$(TOOLS)/bin/nanopb_generator"
	@ln -sf "$(WIT_BINDGEN)" "$(TOOLS)/bin/wit-bindgen"

$(PROTOC):
	@set -e; \
	tmp=$$(mktemp -d); trap 'rm -rf "$$tmp"' EXIT; \
	echo "installing protoc $(PROTOC_VERSION) ($(PROTOC_OS)-$(PROTOC_ARCH))"; \
	mkdir -p "$(TOOLS)"; \
	curl -fsSL -o "$$tmp/protoc.zip" \
		https://github.com/protocolbuffers/protobuf/releases/download/v$(PROTOC_VERSION)/protoc-$(PROTOC_VERSION)-$(PROTOC_OS)-$(PROTOC_ARCH).zip; \
	unzip -q -o "$$tmp/protoc.zip" -d "$(TOOLS)/protoc-$(PROTOC_VERSION)" bin/protoc 'include/*'

# From PyPI, as e2e.yml installs it: nanopb's prebuilt release binaries cover
# neither linux-arm64 nor Intel macOS (the macosx-x86 asset is arm64).
$(NANOPB_GENERATOR):
	@echo "installing nanopb $(NANOPB_VERSION)"
	@python3 -m venv "$(TOOLS)/nanopb-$(NANOPB_VERSION)"
	@"$(TOOLS)/nanopb-$(NANOPB_VERSION)/bin/pip" install --quiet "nanopb==$(NANOPB_VERSION)"

# The release tarball (one per OS and architecture, the binary at its top
# level beside the licences), as e2e.yml fetches the linux one.
$(WIT_BINDGEN):
	@set -e; \
	tmp=$$(mktemp -d); trap 'rm -rf "$$tmp"' EXIT; \
	echo "installing wit-bindgen $(WIT_BINDGEN_VERSION) ($(WIT_BINDGEN_OS)-$(WIT_BINDGEN_ARCH))"; \
	mkdir -p "$(dir $(WIT_BINDGEN))"; \
	curl -fsSL -o "$$tmp/wit-bindgen.tar.gz" \
		https://github.com/bytecodealliance/wit-bindgen/releases/download/v$(WIT_BINDGEN_VERSION)/wit-bindgen-$(WIT_BINDGEN_VERSION)-$(WIT_BINDGEN_ARCH)-$(WIT_BINDGEN_OS).tar.gz; \
	tar -xzf "$$tmp/wit-bindgen.tar.gz" -C "$$tmp"; \
	cp "$$tmp/wit-bindgen-$(WIT_BINDGEN_VERSION)-$(WIT_BINDGEN_ARCH)-$(WIT_BINDGEN_OS)/wit-bindgen" "$(WIT_BINDGEN)"

tools-clean: ## Remove the installed tools
	rm -rf .cache/tools

# The design records index and the book's contents restate each record's
# status, and drifted more than once; hack/design-records.sh holds them to
# the records' own `* Status:` lines, and the prose job runs it first.
design-records: ## Check the design records index and docs/SUMMARY.md against the one-pagers' status lines
	@hack/design-records.sh

# Every alert fails, warnings included (Vale's own exit code is 1 for
# errors only): the line output is empty when the prose is clean, and a run
# that printed nothing but did not exit 0 is a broken configuration, which
# fails too rather than passing as clean.
vale: design-records $(VALE) $(VALE_SYNCED) ## Lint the documentation's prose with the pinned Vale (installs it and syncs the style packages first); any alert fails
	@if out=$$($(VALE) --output=line $(VALE_PATHS)); then status=0; else status=$$?; fi; \
	if [ -n "$$out" ]; then printf '%s\n' "$$out"; echo "vale: $$(printf '%s\n' "$$out" | wc -l | tr -d ' ') alerts"; exit 1; fi; \
	if [ "$$status" -ne 0 ]; then echo "vale: exited $$status with no alerts"; exit "$$status"; fi; \
	echo "vale: no alerts"

$(VALE):
	@set -e; \
	tmp=$$(mktemp -d); trap 'rm -rf "$$tmp"' EXIT; \
	echo "installing vale $(VALE_VERSION) ($(VALE_OS)-$(VALE_ARCH))"; \
	mkdir -p "$(dir $(VALE))"; \
	base=https://github.com/errata-ai/vale/releases/download/v$(VALE_VERSION); \
	asset=vale_$(VALE_VERSION)_$(VALE_OS)_$(VALE_ARCH).tar.gz; \
	curl -fsSL -o "$$tmp/$$asset" "$$base/$$asset"; \
	curl -fsSL -o "$$tmp/checksums.txt" "$$base/vale_$(VALE_VERSION)_checksums.txt"; \
	(cd "$$tmp" && grep " $$asset\$$" checksums.txt | shasum -a 256 -c -); \
	tar -xzf "$$tmp/$$asset" -C "$$tmp" vale; \
	cp "$$tmp/vale" "$(VALE)"

$(VALE_SYNCED): .vale.ini $(VALE)
	@echo "syncing the Vale packages .vale.ini names"
	@$(VALE) sync
	@touch "$@"
