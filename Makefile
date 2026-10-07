# Re-vendoring crossplane/crossplane's proto/fn/v1/run_function.proto (AGENTS.md
# "Changing the scaffold"). Every tracked proto/run_function.proto - the files
# Renovate's version-header rule reads: six templates, six goldens, nine
# examples - is the upstream file byte for byte under a five-line header whose
# first line names the release. A Renovate bump moves that version in every
# header but can neither re-download the file nor regenerate the codecs checked
# in beside it, so its PR is the signal to run `make vendor-proto` on the PR's
# branch, review the diff and comment /e2e.
#
# `make vendor-proto` takes the release from the headers (the rust template's);
# `make vendor-proto VERSION=vX.Y.Z` vendors one Renovate has not proposed.
#
# `make gen-bindings` is the same loop for the c guest's wit-bindgen C
# bindings (examples/hello-c/src/gen, generated from its wit/): it runs the
# pinned wit-bindgen, mirrors the result into the c template and refreshes
# the goldens - after a change to the guest's world, a re-vendoring of the
# WASI WIT, or a wit-bindgen bump (the generated files' header names the
# version; e2e.yml's drift check installs the same one).
.PHONY: vendor-proto vendor-proto-fetch vendor-proto-codecs vendor-proto-goldens gen-bindings tools tools-clean
# The steps rewrite one another's inputs; never run them side by side.
.NOTPARALLEL:

# Only the three generators the guests invoke bare from PATH are pinned here;
# the rest are pinned where each guest builds them: zig-protobuf (and the
# protoc it downloads) by hello-zig's build.zig.zon, protoc-gen-es by
# policy-gate's package-lock.json.
#
# protoc stamps its version into the Python codec, which refuses to load on a
# protobuf runtime older than that stamp, so
# PROTOC_VERSION must not pass the runtime the python template's
# requirements.txt pins (team-tags's is the same file). NANOPB_VERSION is
# the nanopb release hello-c's build.zig.zon compiles and e2e.yml's codec
# drift check installs. WIT_BINDGEN_VERSION is the wit-bindgen-cli release
# that wrote hello-c's checked-in bindings and e2e.yml's bindings drift
# check installs.
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

PROTO_COPIES := $(shell git ls-files '*/proto/run_function.proto')
PROTO_VERSION_FILE := crates/guestfn/templates/rust/proto/run_function.proto
VERSION := $(shell sed -n '1s/.*crossplane\/crossplane \(v[0-9.]*\)\..*/\1/p' $(PROTO_VERSION_FILE))
PROTO_URL := https://raw.githubusercontent.com/crossplane/crossplane/$(VERSION)/proto/fn/v1/run_function.proto

vendor-proto: vendor-proto-fetch vendor-proto-codecs vendor-proto-goldens ## Re-vendor every copy at $(VERSION), regenerate the codecs and the scaffold goldens
	@echo "vendored run_function.proto $(VERSION): review git diff, then comment /e2e on the PR"

# Renovate's header regex reads vX.Y.Z only; anything else would leave the
# copies untracked.
vendor-proto-fetch: ## Overwrite every copy with the upstream file at $(VERSION), keeping each copy's own header
	@expr '$(VERSION)' : 'v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$$' >/dev/null || \
		{ echo "VERSION='$(VERSION)' is not a crossplane/crossplane release (vX.Y.Z)" >&2; exit 1; }
	@set -e; \
	upstream=$$(mktemp); trap 'rm -f "$$upstream"' EXIT; \
	echo "fetching $(PROTO_URL)"; \
	curl -fsSL -o "$$upstream" '$(PROTO_URL)'; \
	for f in $(PROTO_COPIES); do \
		case "$$(head -n 1 "$$f")" in "// Vendored from crossplane/crossplane v"*) ;; \
			*) echo "$$f: line 1 is not the vendored-from header" >&2; exit 1 ;; esac; \
		{ head -n 4 "$$f" | sed '1s#crossplane/crossplane v[0-9.]*\.#crossplane/crossplane $(VERSION).#'; \
			echo; cat "$$upstream"; } > "$$f.tmp"; \
		mv "$$f.tmp" "$$f"; \
		echo "  $$f"; \
	done

# rust and rust-v2 have no checked-in codec (build.rs runs prost-build); the
# zig, c, ts and python codecs are mirrored into their guestfn
# templates, which examples_share_the_scaffold_plumbing holds identical to
# the examples. The ts and python codecs are copied file by file: a gen/ directory
# may hold a __pycache__ the templates must not embed.
vendor-proto-codecs: tools ## Regenerate every checked-in guest codec with the pinned generators and mirror them into the templates
	$(WITH_TOOLS) $(MAKE) -C examples/hello-zig gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-c gen-proto
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

# The bindings are mirrored whole: wit-bindgen writes exactly the three files
# the template carries (function.c, function.h, function_component_type.o).
gen-bindings: tools ## Regenerate hello-c's wit-bindgen C bindings with the pinned wit-bindgen, mirror them into the c template and refresh the goldens
	$(WITH_TOOLS) $(MAKE) -C examples/hello-c gen-bindings
	rm -rf crates/guestfn/templates/c/src/gen && cp -R examples/hello-c/src/gen crates/guestfn/templates/c/src/gen
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
