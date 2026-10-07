# dashboard-bundle

Dashboards as code, delivered as a bundle: a
[Crossplane](https://crossplane.io) composition function in C#, compiled
ahead of time by NativeAOT-LLVM with
[componentize-dotnet](https://github.com/bytecodealliance/componentize-dotnet)
into an **ABI v2** component ([docs/abi-v2.md](../../docs/abi-v2.md)) of
about 4.5 MB, run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm).

Teams publish their Grafana dashboards as one versioned archive, a release
artifact. A `DashboardBundle` composite resource names the bundle, pins its
sha256 and picks a Grafana folder:

```yaml
apiVersion: observability.example.org/v1alpha1
kind: DashboardBundle
metadata:
  name: platform
  namespace: monitoring
spec:
  url: https://artifacts.example.com/dashboards/platform-v1.2.0.zip
  digest: sha256:30fcd2b65e69cf67263d111498348b7879eaf415871db947674b25c31a76defd
  folder: Platform
```

At every reconcile the module fetches the bundle, checks it against the
pin, unpacks its dashboards, walks the tree and composes one ConfigMap per
dashboard for Grafana's dashboard sidecar: labelled
`grafana_dashboard: "1"`, annotated `grafana_folder: Platform`, the
dashboard JSON under one data key, named after the file's path (`nodes/overview.json` in bundle `platform` is
ConfigMap `platform-nodes-overview`). `status.dashboards` records how many
dashboards the bundle held and the digest they came from.

## Why a module

A release artifact is the natural unit for dashboards a team versions
together, but a Composition has no step that reads an archive at reconcile
time and turns its contents into resources. A function can, and here that
function needs no package of its own, only the two capabilities it declares:

- **The pin decides what is applied, not the URL.** The bundle's sha256
  must equal `spec.digest`, so a tag moved to other content is refused, and
  rolling a team's dashboards forward or back is a digest change.
- **It unpacks with the platform's own archive API, into a directory.** The
  module writes the bundle into `/tmp`, extracts each dashboard into a
  directory tree with `ZipArchiveEntry.ExtractToFile` and walks the tree
  with `Directory.EnumerateFiles`, the file-based shape most archive and
  template libraries expect. That `/tmp` is the runtime's **private /tmp**:
  a directory created for this one run, pre-opened in the component at
  `/tmp`, removed when the run ends, and invisible to every other run, so
  nothing one reconcile writes is seen by the next, or by another module.
  It is the only directory the component gets, and it is bounded by the
  operator: the runtime creates it under its `$TMPDIR`, so `TMPDIR` on a
  tmpfs `emptyDir` with a `sizeLimit` caps what one run may write (a full
  tmpfs fails the module's write, not the runtime).
- **Both capabilities are requested and granted, one by one.** The manifest
  (`wasmfn.yaml`) requires `filesystem.privateTmp` and `GET` to the bundle
  host under `/dashboards/`; the runtime grants each only where the
  Composition's `compositionPolicy` and the operator's Cedar policy both
  permit it. The module cannot open a socket: the runtime fetches on its
  behalf after checking the resolved address against its block list, and
  writes one audit line per request.
- **A bad bundle keeps the dashboards in place.** Every refusal below is a
  fatal result, and Crossplane applies nothing from a fatal pipeline, so the
  ConfigMaps composed from the last good bundle stay.

A dashboard is a `.json` file. Anything else a bundle carries - a README
beside the dashboards, the `.DS_Store` files and `__MACOSX/` directory a
zip made on macOS adds (whose AppleDouble `._` twins end in `.json` too) -
is skipped, never extracted, and named in one Warning result (the first
ten files, and how many more), which Crossplane turns into a Warning event
on the composite resource; the module also logs each skipped file at
`warn`, in the runtime's log. The module refuses, with a message naming
the file:

- a bundle whose sha256 is not `spec.digest`, or a `spec.digest` that is not
  `sha256:` and 64 lowercase hex digits;
- an archive that is not a valid zip;
- an entry that would land outside the extraction directory (zip slip:
  `../evil.json`, `/etc/evil.json`), skipped files included. The sandbox
  would confine it anyway - the private `/tmp` is the only directory the
  component has, wasmtime refuses a path that climbs out of it (`EPERM`),
  and .NET, which resolves `..` itself first, finds no directory there -
  but the module checks every entry before it extracts anything;
- a `.json` file that is not a dashboard: not JSON, not an object, no
  `uid` or `title` - it claims to be one;
- a dashboard over 1 MiB, what one ConfigMap holds;
- more than 64 dashboards, or none; more than 1024 entries in all, so a
  bundle of anything else stays bounded too;
- two files that would get the same ConfigMap name, or share a `uid`.

### The archive format

The bundle is a **zip**: `System.IO.Compression` works in this component,
`System.Formats.Tar` does not - the WASI build of .NET ships it as
PlatformNotSupported. Zip needs one fix in the project:
`System.IO.Compression` calls zlib through .NET's native compression shim,
and NativeAOT-LLVM ships both for `wasi-wasm` but links neither, so out of
the box every zip call fails at run time ("Lazy PInvoke resolution is not
supported when targeting WebAssembly"). `DashboardBundle.csproj` binds the
shim's P/Invokes at compile time (`DirectPInvoke`) and links the two
archives. `System.Security.Cryptography` is PlatformNotSupported on WASI as
well, so the pin is checked by a small managed SHA-256 (`src/Sha256.cs`),
which the unit tests hold against .NET's own and the FIPS 180-4 vectors.

## Run it

The toolchain is the **.NET 10 SDK**. The project references
componentize-dotnet 0.8.0-preview00011 and the NativeAOT-LLVM build it was
released against (10.0.0-rc.1.26306.1, from the `dotnet-experimental` feed
that `nuget.config` adds); componentize-dotnet runs wit-bindgen over `wit/`
on every build and downloads the WASI SDK 29 on the first one.
NativeAOT-LLVM publishes its compiler for linux-x64, linux-arm64 and win-x64
hosts only, so `make` builds with the host's `dotnet` on Linux and in the
`mcr.microsoft.com/dotnet/sdk:10.0` container everywhere else (macOS),
keeping NuGet packages and the WASI SDK in the `function-wasm-dotnet` Docker
volume. `make build DOTNET=dotnet` forces the host's own; `make test` and
`make lint` make the same choice, and the tests also run under a macOS
`dotnet` (`make test DOTNET=dotnet`).

```shell
make test          # the unit tests under dotnet test (MSTest)
make lint          # dotnet format --verify-no-changes, protoc's codec left out
make build         # fn.wasm, a component
make render        # serve it with function-wasm and crossplane render example/xr.yaml
make render-check  # the render test, example/xprin.yaml (needs xprin)
make bundles       # repack the fixture bundles after editing a dashboard
```

A local render needs no internet. `example/` holds:

- `definition.yaml`: the `DashboardBundle` XRD (Crossplane v2, namespaced).
- `xr.yaml` and `xr-payments.yaml`: two teams' bundles, in their own
  namespaces and folders.
- `composition.yaml`: one pipeline step, the module served from this
  directory under `example/wasmfn.local.yaml`, a manifest that asks for the
  fixture server instead of `artifacts.example.com`.
- `bundles/`: the dashboards the fixture bundles are packed from, and
  `pack.py`, which `make bundles` runs to repack them and print the digests
  to pin.
- `fixtures/bundles/`: the two bundles, which `../render.sh` serves on
  `127.0.0.1:9480`. The platform bundle carries a `README.md` beside its
  dashboards: the render composes nothing for it and prints the warning.
- `policy.cedar`: the operator policy the runtime runs under, permitting the
  private `/tmp`, the fixture host and dialling loopback, which the egress
  block list refuses by default. Without its `usePrivateTmp` permit,
  `function validate --resolve` (which `../render.sh` runs first) refuses
  the step: `requires a private /tmp (requires.filesystem.privateTmp), which
  the operator policy (--sandbox-policy-file) does not permit for this
  request`.
- `xprin.yaml`: the render test, one case per XR, without schema validation
  (a ConfigMap is a core kind, with no CRD to validate against).

## In a cluster

Name your artifact host in `wasmfn.yaml`, publish the module with its
manifest, then reference the digest `guestfn push` prints:

```shell
make build
guestfn push ghcr.io/example/dashboard-bundle:v0.1.0
```

```yaml
  - step: dashboard-bundle
    functionRef:
      name: function-wasm
    input:
      apiVersion: wasm.fn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/dashboard-bundle:v0.1.0@sha256:…
```

The operator grants both capabilities in the runtime's
`--sandbox-policy-file`, and bounds the private `/tmp` through the runtime's
`DeploymentRuntimeConfig` (`TMPDIR` on a tmpfs `emptyDir` with a
`sizeLimit`, as function-wasm's [runtime flags](https://jonasz-lasut.github.io/function-wasm/operators/runtime-flags.html) page shows):

```cedar
permit (principal, action == Action::"usePrivateTmp", resource);

permit (principal, action == Action::"grantEgress", resource == HostPattern::"artifacts.example.com")
when { context.method == "GET" };
```

The host's egress budgets apply: a bundle is at most 4 MiB, the largest
response the runtime hands a module. Grafana's chart picks the ConfigMaps
up with its dashboard sidecar watching every namespace and creating folders
from the annotation:

```yaml
sidecar:
  dashboards:
    enabled: true
    label: grafana_dashboard
    labelValue: "1"
    folderAnnotation: grafana_folder
    searchNamespace: ALL
    provider:
      foldersFromFilesStructure: true
```

## Layout

- `src/Function.cs` is the function over the protobuf messages: the
  composite resource's spec, the fetch, the pin, the ConfigMaps and the
  status. `src/Bundle.cs` checks, unpacks and walks the bundle;
  `src/Sha256.cs` is the pin's hash. All natively testable (`make test`)
  with a fetch double and a temporary directory in place of the private
  `/tmp`.
- `src/App.cs` (wasm only) is the world wiring over wit-bindgen's generated
  bindings: the `run` export, the typed log adapter and the blocking
  `wasi:http` fetch.
- `wit/world.wit` is this guest's world, in a `local:guest` package of its
  own: the wasmfn contract with a sync `run`, because wit-bindgen's C# async
  bindings (0.58 through 0.62) do not compile for this world yet - a
  sync-lifted function satisfies the runtime's async world - plus the
  `wasi:http@0.2` import it fetches with; `wit/deps/` carries the WASI 0.2
  WIT that import needs. The .NET runtime inside the component imports
  WASI 0.2.6 for itself, the filesystem the private `/tmp` arrives through
  included.
- `proto/run_function.proto` is crossplane's `RunFunction` contract,
  vendored; the checked-in `src/Gen/RunFunction.cs` is protoc's output
  (`make gen-proto` to redo) over
  [Google.Protobuf](https://www.nuget.org/packages/Google.Protobuf).
- `DashboardBundle.csproj` builds the component; `test/` is the native test
  project, which compiles `src/` without `src/App.cs`. Package versions live
  in `Directory.Packages.props`.

C# has no `guestfn init` flavour: `guestfn build` does not detect this
project, so `make build` builds it, and `guestfn push` publishes the result
like any other module.
