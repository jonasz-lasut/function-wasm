# policy-gate

The organisation's policy as the last step of every pipeline: a
[Crossplane](https://crossplane.io) composition function in TypeScript,
componentized with [jco](https://github.com/bytecodealliance/jco) into a
WebAssembly **component** (ABI v2) and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm).

A team's Composition composes what it needs - here, an RDS `Instance` for a
`Database` composite resource. The gate runs after it and holds every
composed resource to the policy of the environment the XR names in
`spec.environment`:

- it adds the environment's **mandatory tags** to each composed resource's
  `spec.forProvider.tags`, never overwriting a tag the team set;
- it refuses a **region** or an **instance class** the environment does not
  allow with a fatal result naming the resource and the field, so Crossplane
  applies nothing;
- it lets softer findings through with a **warning** - a team tag that
  overrides one of the environment's - and records what it did in
  `status.policy` on the XR.

```yaml
apiVersion: data.example.org/v1alpha1
kind: Database
metadata:
  name: orders
  namespace: default
spec:
  environment: prod        # the EnvironmentConfig whose policy applies
  region: eu-central-1
  instanceClass: db.r6g.large
  tags:
    team: orders
```

The rules are data, one `EnvironmentConfig` per environment, owned by the
platform team:

```yaml
apiVersion: apiextensions.crossplane.io/v1beta1
kind: EnvironmentConfig
metadata:
  name: prod
data:
  policy:
    mandatoryTags:
      cost-center: "4200"
      data-classification: confidential
    allowedRegions: [eu-central-1, eu-west-1]
    allowedInstanceClasses: [db.r6g.large, db.r6g.xlarge, db.r6g.2xlarge]
```

An unset allow list leaves that field unrestricted. The checks apply to the
fields a resource sets: a region left to the provider configuration cannot
be judged, and most kinds have no instance class.

## Why a module

Mandatory tags across every Composition are a long-standing request
([crossplane/crossplane#1225](https://github.com/crossplane/crossplane/issues/1225),
[discussion #2617](https://github.com/crossplane/crossplane/discussions/2617),
[provider-upjet-aws#169](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169)
for provider-level default tags). A shared step does it once for every
Composition; as a function-wasm module, that step is the platform team's own:

- **The policy rolls out on its own.** Each Composition pins the gate by
  digest. The platform team publishes a new gate with `guestfn push` and
  moves the digest; nothing is installed in the cluster, the teams' steps
  stay as they are, and a rollback is the old digest. Changing a rule -
  a new tag, another allowed region - is an edit to an EnvironmentConfig,
  not a new module.
- **The rules come through `requirements`, like a native function's.** On
  its first call the gate only asks for the `EnvironmentConfig` named after
  `spec.environment`; Crossplane fetches it and calls the gate again with
  it. function-wasm forwards the request and the response at the wire
  level, so the round trip needs nothing from the runtime, and the module
  needs no cluster access of its own.
- **It fails closed.** A missing EnvironmentConfig, a malformed policy, an
  XR without an environment, or a Crossplane that announces it cannot
  supply required resources is a fatal result, never a pass.
- **It asks for nothing.** The manifest (`wasmfn.yaml`) requires no
  capability, and the component is built without `wasi:http`
  (`jco componentize -d http -d fetch-event`), so the gate could not reach
  the network even with a grant.

## Run it

The toolchain is node and npm: protobuf-es for the codec, `tsc --noEmit` as
the type gate (node runs the tests by stripping types natively), esbuild to
bundle, jco to componentize.

```shell
npm install
npm test               # tsc --noEmit, then the unit tests natively under node
make build             # fn.wasm, a component (npm run build)
make render            # serve it with function-wasm and crossplane render example/xr.yaml
make render-check      # the render test, example/xprin.yaml (needs xprin)
```

A local render needs no cluster and no AWS account; the first step,
function-go-templating, runs in Docker. `example/` holds:

- `definition.yaml`: the `Database` XRD (Crossplane v2, namespaced).
- `xr.yaml` and `xr-dev.yaml`: a prod database and a dev one, whose team
  sets its own cost center.
- `composition.yaml`: function-go-templating composes the instance, then the
  gate runs, served from this directory.
- `required-resources.yaml`: the `prod` and `dev` EnvironmentConfigs, which
  render hands the gate when it asks for one.
- `crds/`: provider-upjet-aws's RDS `Instance` CRD, vendored, so the render
  test validates what the composition composes against the provider's own
  schema.
- `xprin.yaml`: the render test, one case per XR: the tags added and kept,
  and `status.policy`.

A request the policy refuses ends in a fatal result, which fails a render,
so the unit tests (`test/fn.test.ts`) cover every fatal path, along with the
requirement round trip, the tags and the warnings.

## In a cluster

Each release publishes this example, signed keyless and attested, as
`ghcr.io/jonasz-lasut/wasmfn/examples/policy-gate:v<version>` (the version in
`wasmfn.yaml`); the release's `function-wasm-examples-<release>.yaml` lists
its reference pinned by digest. To run your own build instead, publish it
with its manifest, then reference the digest `guestfn push` prints as the last
step of each Composition:

```shell
guestfn push ghcr.io/example/policy-gate:v0.1.0
```

```yaml
  - step: policy-gate
    functionRef:
      name: function-wasm
    input:
      apiVersion: wasm.fn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/policy-gate:v0.1.0@sha256:…
```

Apply one EnvironmentConfig per environment. The gate needs no operator
policy: it requires no capability.

## Layout

- `src/policy.ts`: the policy over plain JSON - reading `data.policy`,
  checking a resource, adding its tags.
- `src/fn.ts`: the gate over the protobuf-es messages - the requirement,
  the fatal results, the warnings and `status.policy` - natively testable
  (`npm test`).
- `src/main.ts` (wasm only): the world wiring, the `run` export. The world's
  root-level `log` import arrives as a **default import from a module named
  after it** (`import log from "log"`, typed by `src/log.d.ts`), and esbuild
  must keep it external (`--external:log`).
- `wit/world.wit`: this guest's own world, in a `local:guest` package: the
  [ABI v2 world](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi-v2.md)
  restated with `run` declared **sync**, because componentize-js cannot
  async-lift a custom world's export yet, and a sync-lifted function
  satisfies the runtime's async world.
- `proto/run_function.proto`, `src/gen/` and `src/log.d.ts` are the ts
  scaffold's plumbing (`guestfn init --lang ts`), kept identical to it:
  crossplane's `RunFunction` contract, vendored, and protobuf-es's `js+dts`
  output for it, checked in (`npm run gen-proto` redoes it, needs `protoc`).

The official
[function-sdk-typescript](https://github.com/crossplane/function-sdk-typescript)
is deliberately not used: it targets the native deployment shape (a node
gRPC server), and its generated codec module itself imports @grpc/grpc-js,
which cannot exist inside a SpiderMonkey component.
