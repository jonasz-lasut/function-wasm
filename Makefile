# guestfn's Makefile. Two kinds of targets live here:
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
PROJECT_NAME := guestfn
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
# make e2e            up, every profile in turn, down (the library's e2e
#                     pipeline: e2e.init, e2e.run, e2e.done)
# make e2e.up         one cluster for both runtimes: kind + Crossplane
#                     (controlplane.up, then e2e.crossplane waits for its
#                     deployment and the package CRDs its init container
#                     registers), the suite's setup (crates/e2e's e2e-setup
#                     binary: the registry with its TLS and auth, provider-nop,
#                     the test module pushed and signed under every variant,
#                     the profiles' ConfigMaps, Secrets and runtime configs,
#                     the kvm device plugin when the node has /dev/kvm, the
#                     XRD), function-wasm's image and package installed from
#                     it, and function-oci beside it when the kind node has
#                     /dev/kvm (E2E_OCI=auto, the default; 1 requires it, 0
#                     leaves it out, what CI's two cluster jobs set)
# make e2e.wasm-profile.<p>
#                     one wasm profile's suite (default, strict, lifecycle):
#                     cargo test --test wasm_<p>, which switches function-wasm
#                     to that profile's runtime config first
# make e2e.oci-profile.<p>
#                     one oci profile's suite (default): cargo test --test
#                     oci_<p>, which switches function-oci the same way
# make e2e.test.wasm  the wasm profiles in turn; e2e.test.oci the oci ones;
#                     e2e.test both, the oci ones when the node has /dev/kvm
# make e2e.down       delete the cluster
# make e2e.dump       the cluster's state for a failed run, the function pods
#                     described and logged first
# make e2e.guest      rebuild the committed test module, crates/e2e/guest/fn.wasm
# make e2e.oci.guest  build the test module as a process (musl), the oci
#                     profiles' image content
# make oci.tools      fetch the pinned Firecracker and guest kernel
#                     (hack/oci-tools.sh, the pins read from Dockerfile.oci)
# make oci.init       build the guest init and the fixture static for musl
# make oci.test       the OCI engine's suite over a real Firecracker VM (KVM,
#                     a Linux host: the test process spawns Firecracker)
# make print-crossplane-version
#                     the CROSSPLANE_VERSION in effect (the release matrix
#                     reads the default this way)
#
# FUNCTION_BIN names a prebuilt linux runtime binary for the host's
# architecture (CI's build-tools job hands one over), wrapped by
# Dockerfile.wasm.dev; without it the image is built from Dockerfile.wasm, which
# compiles the workspace in a container. FUNCTION_OCI_TOOLS names a
# directory of prebuilt function (the oci engine), init, firecracker and
# vmlinux (CI's build-oci job hands one over), wrapped by Dockerfile.oci.dev;
# without it they are built and fetched here.
E2E_IMAGE := function-wasm-e2e:dev
E2E_OCI_IMAGE := function-oci-e2e:dev
E2E_DIR := $(OUTPUT_DIR)/e2e
# The profiles, one test target each: crates/e2e/tests/<runtime>_<profile>.rs
# over crates/e2e/profiles/<runtime>/<profile>/.
E2E_WASM_PROFILES := default strict lifecycle
E2E_OCI_PROFILES := default
E2E_CARGO := cargo run -q -p guestfn-e2e --features cluster
E2E_GUEST_DIR := crates/e2e/guest
FUNCTION_PKG_EXAMPLES := examples/pdb-addon/example
# The image and the runtime config local.xpkg.deploy.function.% installs a
# Function with; the function-oci target sets its own below.
FUNCTION_IMAGE := $(E2E_IMAGE)
FUNCTION_RUNTIME_CONFIG := e2e-wasm-default
# Whether function-oci is part of the cluster: `auto` (the default)
# installs it when the kind node has /dev/kvm, which the pod opens there
# and the node inherits from the host (a Linux host with KVM has it, and
# so does Colima with nested virtualization; Docker Desktop has not); 1
# requires it and 0 leaves it out, which CI's two cluster jobs set.
E2E_OCI ?= auto
E2E_NODE_HAS_KVM := docker exec $(KIND_CLUSTER_NAME)-control-plane test -c /dev/kvm
e2e_oci_wanted = { [ "$(E2E_OCI)" = "1" ] || { [ "$(E2E_OCI)" = "auto" ] && $(E2E_NODE_HAS_KVM) 2>/dev/null; }; }

# The OCI runtime's pieces (docs/one-pager-oci-runtime.md): the Firecracker
# release and the guest kernel its CI publishes, fetched by hack/oci-tools.sh
# at the versions and checksums Dockerfile.oci pins (the one place they are
# written), and the engine crate's two binaries, the init and the test
# fixture, built static for musl. Every one of them is a Linux binary: a
# Linux host builds them natively, any other host through Docker (the
# image from Dockerfile.oci, the test module as a process in a rust
# container), and oci.test runs on Linux alone.
OCI_ARCH := $(if $(filter arm64 aarch64,$(shell uname -m)),aarch64,x86_64)
MUSL_TARGET := $(OCI_ARCH)-unknown-linux-musl
OCI_TOOLS := .cache/tools/oci
HOST_IS_LINUX := $(filter Linux,$(shell uname -s))
# The guest built in a container lands under its own target directory, so
# the host's own builds of the guest never mix with Linux ones.
E2E_OCI_GUEST ?= $(E2E_GUEST_DIR)/$(if $(HOST_IS_LINUX),target,target/linux)/$(MUSL_TARGET)/release/e2e-guest-oci

.PHONY: e2e.up e2e.crossplane e2e.setup e2e.image e2e.test e2e.test.wasm e2e.test.oci e2e.down e2e.guest e2e.dump xpkg.build.function-wasm print-crossplane-version
.PHONY: oci.tools oci.init oci.test e2e.oci.guest e2e.oci.image e2e.oci.install xpkg.build.function-oci
print-crossplane-version:
	@echo $(CROSSPLANE_VERSION)

e2e.init: e2e.up
e2e.run: e2e.test
e2e.done: e2e.down

# The steps after the cluster run through $(MAKE), in order: the oci ones
# depend on what the node has, which is known once the cluster exists
# (setup fails under E2E_OCI=1 on a node without /dev/kvm).
e2e.up: controlplane.up e2e.crossplane
	@if $(e2e_oci_wanted); then [ -f $(E2E_OCI_GUEST) ] || $(MAKE) e2e.oci.guest; fi
	@$(MAKE) e2e.setup xpkg.build.function-wasm local.xpkg.deploy.function.function-wasm
	@$(INFO) waiting for function-wasm to become healthy
	@$(KUBECTL) wait function.pkg.crossplane.io/function-wasm --for=condition=Healthy --timeout=300s || $(FAIL)
	@if $(e2e_oci_wanted); then $(MAKE) e2e.oci.install; \
	else echo "function-oci is left out (E2E_OCI=$(E2E_OCI): auto installs it when the kind node has /dev/kvm, 1 requires it); the oci profiles are skipped"; fi
	@$(OK) the end-to-end cluster is up: make e2e.test runs every profile, make e2e.wasm-profile.default or e2e.oci-profile.default one

# controlplane.up returns as soon as the chart is installed; Crossplane's
# init container registers the package CRDs after that, and an older
# release takes long enough for the setup's Provider to meet a 404.
e2e.crossplane: $(KUBECTL)
	@$(INFO) waiting for crossplane and its package api
	@$(KUBECTL) wait --namespace $(CROSSPLANE_NAMESPACE) --for=condition=Available --timeout=300s deployment/crossplane || $(FAIL)
	@$(KUBECTL) wait --for=condition=Established --timeout=120s crd/providers.pkg.crossplane.io crd/functions.pkg.crossplane.io crd/deploymentruntimeconfigs.pkg.crossplane.io || $(FAIL)
	@$(OK) waiting for crossplane and its package api

# E2E_OCI_GUEST, when the file exists, adds the oci image and variants to
# what setup pushes; KIND_CLUSTER_NAME names the node setup reads the
# /dev/kvm group from, E2E_OCI what it does about the device.
e2e.setup:
	@$(INFO) setting up the registry, provider-nop, the test module and the profiles
	@PROVIDER_NOP_VERSION=$(PROVIDER_NOP_VERSION) KIND_CLUSTER_NAME=$(KIND_CLUSTER_NAME) E2E_OCI=$(E2E_OCI) $(if $(wildcard $(E2E_OCI_GUEST)),E2E_OCI_GUEST=$(E2E_OCI_GUEST),) $(E2E_CARGO) --bin e2e-setup || $(FAIL)
	@$(OK) setting up the registry, provider-nop, the test module and the profiles

e2e.image:
	@$(INFO) building the runtime image $(E2E_IMAGE) for linux/$(TARGETARCH)
ifdef FUNCTION_BIN
	@mkdir -p $(E2E_DIR)/prebuilt && cp $(FUNCTION_BIN) $(E2E_DIR)/prebuilt/function
	@docker buildx build --load --platform linux/$(TARGETARCH) -f Dockerfile.wasm.dev -t $(E2E_IMAGE) $(E2E_DIR)/prebuilt || $(FAIL)
else
	@docker buildx build --load --platform linux/$(TARGETARCH) -f Dockerfile.wasm --target image -t $(E2E_IMAGE) . || $(FAIL)
endif
	@$(OK) building the runtime image $(E2E_IMAGE)

# The Function package from the image: what xpkg.mk's xpkg.build.% would do
# for a Provider. --examples-root must hold Crossplane objects only (xpkg
# build parses every YAML file under it), which the pdb-addon example's
# directory does.
xpkg.build.function-wasm: $(CROSSPLANE_CLI) e2e.image
	@$(INFO) building package function-wasm-$(VERSION).xpkg
	@mkdir -p $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)
	@$(CROSSPLANE_CLI) xpkg build --package-root package/wasm --examples-root $(FUNCTION_PKG_EXAMPLES) \
		--embed-runtime-image $(E2E_IMAGE) \
		--package-file $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)/function-wasm-$(VERSION).xpkg || $(FAIL)
	@$(OK) building package function-wasm-$(VERSION).xpkg

# local.xpkg.deploy.provider.%'s shape for a Function: the runtime image
# loaded into the kind node and the Function installed from the synced cache
# with packagePullPolicy Never, on its runtime's default profile (setup
# applied every profile's DeploymentRuntimeConfig).
local.xpkg.deploy.function.%: $(KIND) $(KUBECTL) local.xpkg.sync
	@$(INFO) deploying function package $* $(VERSION)
	@$(KIND) load docker-image $(FUNCTION_IMAGE) -n $(KIND_CLUSTER_NAME) || $(FAIL)
	@echo '{"apiVersion":"pkg.crossplane.io/v1","kind":"Function","metadata":{"name":"$*"},"spec":{"package":"xpkg.crossplane.internal/dev/$*@$(LOCAL_XPKG_DIGEST)","skipDependencyResolution":true,"packagePullPolicy":"Never","runtimeConfigRef":{"name":"$(FUNCTION_RUNTIME_CONFIG)"}}}' | $(KUBECTL) apply -f - || $(FAIL)
	@$(OK) deploying function package $* $(VERSION)

# The OCI runtime's targets.
oci.tools: ## Fetch the pinned Firecracker release and guest kernel under .cache/tools/oci
	@hack/oci-tools.sh $(OCI_ARCH) $(OCI_TOOLS) || $(FAIL)

oci.init: ## Build the guest init and the test fixture (the engine crate's binaries) static for musl
	@rustup target add $(MUSL_TARGET) >/dev/null
	@cargo build --release -p guestfn-engine --no-default-features --bins --target $(MUSL_TARGET) || $(FAIL)

oci.test: oci.tools oci.init ## The OCI engine's suite over a real Firecracker VM (needs /dev/kvm)
	@GUESTFN_FIRECRACKER=$(CURDIR)/$(OCI_TOOLS)/firecracker GUESTFN_KERNEL=$(CURDIR)/$(OCI_TOOLS)/vmlinux \
	 GUESTFN_INIT=$(CURDIR)/target/$(MUSL_TARGET)/release/guestfn-init \
	 GUESTFN_FIXTURE=$(CURDIR)/target/$(MUSL_TARGET)/release/guestfn-guest-fixture GUESTFN_VM_CONSOLE=1 \
	 cargo test -p guestfn-engine --no-default-features --features oci -- --nocapture || $(FAIL)

# The test module as a process, static for musl, on the guest's own pinned
# toolchain (its rust-toolchain.toml). A Linux host builds it natively. Any
# other host builds it in a rust container over the whole tree, since the
# guest reads the rust scaffold's proto in place and prost-build shells out
# to protoc (Debian keeps the well-known types' .proto files in
# libprotobuf-dev, apart from the compiler). The cargo registry and the whole rustup home stay in named
# volumes, so the second build is warm; rustup unpacks a toolchain under
# its home's tmp and renames it into place, which has to be one filesystem,
# so the volume holds the home entire and the pinned toolchain is installed
# into it on the first run.
e2e.oci.guest:
	@$(INFO) building $(E2E_OCI_GUEST)
ifneq ($(HOST_IS_LINUX),)
	@cd $(E2E_GUEST_DIR) && rustup target add $(MUSL_TARGET) >/dev/null && cargo build --release --features process --bin e2e-guest-oci --target $(MUSL_TARGET) || $(FAIL)
else
	@docker run --rm -v $(CURDIR):/src -w /src/$(E2E_GUEST_DIR) \
		-v guestfn-cargo-registry:/usr/local/cargo/registry -v guestfn-rustup:/usr/local/rustup \
		-e CARGO_TARGET_DIR=/src/$(E2E_GUEST_DIR)/target/linux -e RUSTUP_AUTO_INSTALL=1 -e DEBIAN_FRONTEND=noninteractive rust:1 \
		sh -c 'apt-get update -qq && apt-get install -y -qq --no-install-recommends protobuf-compiler libprotobuf-dev >/dev/null && rustup target add $(MUSL_TARGET) && cargo build --release --features process --bin e2e-guest-oci --target $(MUSL_TARGET)' || $(FAIL)
endif
	@$(OK) building $(E2E_OCI_GUEST)

# The function-oci image: Dockerfile.oci.dev around the four files
# FUNCTION_OCI_TOOLS names (CI's build job hands them over), or
# Dockerfile.oci built in Docker, which compiles the runtime and the init
# and fetches Firecracker and the kernel at its own pins - the same default
# as e2e.image, so no host builds a Linux binary itself.
e2e.oci.image:
	@$(INFO) building the runtime image $(E2E_OCI_IMAGE) for linux/$(TARGETARCH)
ifdef FUNCTION_OCI_TOOLS
	@mkdir -p $(E2E_DIR)/prebuilt-oci
	@cp $(FUNCTION_OCI_TOOLS)/function $(FUNCTION_OCI_TOOLS)/init $(FUNCTION_OCI_TOOLS)/firecracker $(FUNCTION_OCI_TOOLS)/vmlinux $(E2E_DIR)/prebuilt-oci/
	@docker buildx build --load --platform linux/$(TARGETARCH) -f Dockerfile.oci.dev -t $(E2E_OCI_IMAGE) $(E2E_DIR)/prebuilt-oci || $(FAIL)
else
	@docker buildx build --load --platform linux/$(TARGETARCH) -f Dockerfile.oci --target image -t $(E2E_OCI_IMAGE) . || $(FAIL)
endif
	@$(OK) building the runtime image $(E2E_OCI_IMAGE)

xpkg.build.function-oci: $(CROSSPLANE_CLI) e2e.oci.image
	@$(INFO) building package function-oci-$(VERSION).xpkg
	@mkdir -p $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)
	@$(CROSSPLANE_CLI) xpkg build --package-root package/oci --examples-root $(FUNCTION_PKG_EXAMPLES) \
		--embed-runtime-image $(E2E_OCI_IMAGE) \
		--package-file $(XPKG_OUTPUT_DIR)/linux_$(TARGETARCH)/function-oci-$(VERSION).xpkg || $(FAIL)
	@$(OK) building package function-oci-$(VERSION).xpkg

local.xpkg.deploy.function.function-oci: FUNCTION_IMAGE := $(E2E_OCI_IMAGE)
local.xpkg.deploy.function.function-oci: FUNCTION_RUNTIME_CONFIG := e2e-oci-default

# function-oci beside function-wasm, on a node with /dev/kvm: setup installed
# the device plugin and put the node's kvm group into the oci runtime config
# (docs/oci/install.md, "Node requirements").
e2e.oci.install: xpkg.build.function-oci local.xpkg.deploy.function.function-oci
	@$(INFO) waiting for function-oci to become healthy
	@$(KUBECTL) wait function.pkg.crossplane.io/function-oci --for=condition=Healthy --timeout=300s || $(FAIL)
	@$(OK) waiting for function-oci to become healthy

# The profiles: one explicit target each (a phony target is never matched
# by a pattern rule), e2e.<runtime>-profile.<p>.
define e2e.profile
.PHONY: e2e.$(1)-profile.$(2)
e2e.$(1)-profile.$(2):
	@$$(INFO) running the $(1) $(2) profile suite
	@cargo test -p guestfn-e2e --features cluster --test $(1)_$(2) || $$(FAIL)
	@$$(OK) running the $(1) $(2) profile suite
endef
$(foreach p,$(E2E_WASM_PROFILES),$(eval $(call e2e.profile,wasm,$(p))))
$(foreach p,$(E2E_OCI_PROFILES),$(eval $(call e2e.profile,oci,$(p))))

e2e.test.wasm: $(addprefix e2e.wasm-profile.,$(E2E_WASM_PROFILES))
e2e.test.oci: $(addprefix e2e.oci-profile.,$(E2E_OCI_PROFILES))

# The wasm profiles under their old names, e2e.test.<p>: a comment-started
# /e2e run reads e2e.yml from main, which calls these until the workflow
# with the per-runtime names has merged. Remove them after that.
define e2e.test.alias
.PHONY: e2e.test.$(1)
e2e.test.$(1): e2e.wasm-profile.$(1)
endef
$(foreach p,$(E2E_WASM_PROFILES),$(eval $(call e2e.test.alias,$(p))))
e2e.test: e2e.test.wasm
	@if $(e2e_oci_wanted); then $(MAKE) e2e.test.oci; \
	else echo "function-oci is not in this cluster (E2E_OCI=$(E2E_OCI)); the oci profiles are skipped"; fi

e2e.down: controlplane.down

# The cluster's state for a failed run: every function pod described and
# logged (the previous container's too) into the output, where the dump
# itself is an artifact, then controlplane.dump plus the suite's own logs
# under _output/e2e/logs; CI uploads the directory.
e2e.dump: $(KUBECTL)
	@$(KUBECTL) get pods --all-namespaces -o wide || true
	@for p in $$($(KUBECTL) -n crossplane-system get pods -l pkg.crossplane.io/function -o name); do \
		echo "==== $$p"; \
		$(KUBECTL) -n crossplane-system describe $$p || true; \
		echo "---- $$p logs"; \
		$(KUBECTL) -n crossplane-system logs $$p --all-containers --tail=200 || true; \
		echo "---- $$p previous logs"; \
		$(KUBECTL) -n crossplane-system logs $$p --all-containers --previous --tail=200 2>/dev/null || true; \
	done
	@$(MAKE) controlplane.dump

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
GO_MODULE := github.com/jonasz-lasut/guestfn/examples/pdb-addon
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
