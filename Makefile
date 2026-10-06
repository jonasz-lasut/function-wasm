# Re-vendoring crossplane/crossplane's proto/fn/v1/run_function.proto (AGENTS.md
# "Changing the scaffold"). Every tracked proto/run_function.proto - the files
# Renovate's version-header rule reads: six templates, six goldens, eight
# examples - is the upstream file byte for byte under a five-line header whose
# first line names the release. A Renovate bump moves that version in every
# header but can neither re-download the file nor regenerate the codecs checked
# in beside it, so its PR is the signal to run `make vendor-proto` on the PR's
# branch, review the diff and comment /e2e.
#
# `make vendor-proto` takes the release from the headers (the rust template's);
# `make vendor-proto VERSION=vX.Y.Z` vendors one Renovate has not proposed.
.PHONY: vendor-proto vendor-proto-fetch vendor-proto-codecs vendor-proto-goldens tools tools-clean
# The steps rewrite one another's inputs; never run them side by side.
.NOTPARALLEL:

# Only the two generators the guests invoke bare from PATH are pinned here;
# the rest are pinned where each guest builds them: protoc-gen-go and
# protoc-gen-go-vtproto by examples/hello-tinygo/go.mod, zig-protobuf (and the
# protoc it downloads) by hello-zig's build.zig.zon, as-proto-gen and
# protoc-gen-es by the package-lock.json files.
#
# protoc stamps its version into the AssemblyScript and Python codecs, and the
# Python one refuses to load on a protobuf runtime older than that stamp, so
# PROTOC_VERSION must not pass the runtime the python template's
# requirements.txt pins (hello-python's is the same file). NANOPB_VERSION is
# the nanopb release hello-c's build.zig.zon compiles and e2e.yml's codec
# drift check installs.
PROTOC_VERSION := 35.1
NANOPB_VERSION := 0.4.9.1

PROTOC_OS := $(if $(filter Darwin,$(shell uname -s)),osx,linux)
PROTOC_ARCH := $(if $(filter arm64 aarch64,$(shell uname -m)),aarch_64,x86_64)
TOOLS := $(CURDIR)/.cache/tools/$(PROTOC_OS)-$(PROTOC_ARCH)
PROTOC := $(TOOLS)/protoc-$(PROTOC_VERSION)/bin/protoc
NANOPB_GENERATOR := $(TOOLS)/nanopb-$(NANOPB_VERSION)/bin/nanopb_generator
# Only the two pinned binaries, never the nanopb venv's bin/ (its python3 would
# shadow the system one), go in front of PATH.
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
# tinygo, zig, c, ts and python codecs are mirrored into their guestfn
# templates, which render-matches-the-examples holds identical to the
# examples. The ts and python codecs are copied file by file: a gen/ directory
# may hold a __pycache__ the templates must not embed.
vendor-proto-codecs: tools ## Regenerate every checked-in guest codec with the pinned generators and mirror them into the templates
	$(WITH_TOOLS) $(MAKE) -C examples/hello-tinygo generate
	$(WITH_TOOLS) $(MAKE) -C examples/hello-zig gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-c gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-assemblyscript gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-python gen-proto
	$(WITH_TOOLS) $(MAKE) -C examples/hello-ts gen-proto
	for f in run_function.pb.go run_function_vtproto.pb.go; do \
		cp "examples/hello-tinygo/internal/fnv1/$$f" "crates/guestfn/templates/tinygo/internal/fnv1/$$f.tmpl"; \
	done
	rm -rf crates/guestfn/templates/zig/src/fnv1 && cp -R examples/hello-zig/src/fnv1 crates/guestfn/templates/zig/src/fnv1
	rm -rf crates/guestfn/templates/c/src/fnv1 && cp -R examples/hello-c/src/fnv1 crates/guestfn/templates/c/src/fnv1
	for f in run_function_pb.js run_function_pb.d.ts; do \
		cp "examples/hello-ts/src/gen/$$f" "crates/guestfn/templates/ts/src/gen/$$f"; \
	done
	cp examples/hello-python/src/gen/run_function_pb2.py crates/guestfn/templates/python/src/gen/run_function_pb2.py

vendor-proto-goldens: ## Refresh the guestfn scaffold goldens from the templates
	UPDATE_GOLDENS=1 cargo test -p guestfn

tools: $(PROTOC) $(NANOPB_GENERATOR) ## Install the pinned protoc and nanopb_generator under .cache/tools
	@mkdir -p "$(TOOLS)/bin"
	@ln -sf "$(PROTOC)" "$(TOOLS)/bin/protoc"
	@ln -sf "$(NANOPB_GENERATOR)" "$(TOOLS)/bin/nanopb_generator"

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

tools-clean: ## Remove the installed tools
	rm -rf .cache/tools
