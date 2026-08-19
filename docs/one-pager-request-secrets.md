# Request-Delivered Secrets and Files

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.0 (env retyped and envFrom; files not implemented)

A module that talks to a cloud API through `wasmfn.HTTPClient()` usually
does so through an SDK, and SDKs find their credentials where they always
have: in the environment (`AWS_ACCESS_KEY_ID`, `VAULT_TOKEN`) or in a file
the environment points at (`GOOGLE_APPLICATION_CREDENTIALS`, `KUBECONFIG`,
`AWS_SHARED_CREDENTIALS_FILE`). Today the guest receives step credentials
inside its `RunFunctionRequest` and would have to copy them into its own
environment or `/tmp` before the SDK's auth chain runs — possible in Go,
awkward in TinyGo and Rust, repeated by every module author. This document
designs `sandbox` grants that let the *runtime* do that copy, from the
request and only from the request: environment variables whose value is a
key of a step credential, and files written into the private `/tmp` before
the run. Nothing comes from the host's filesystem or environment,
consistent with the decision that host directories are not mountable. It
also settles the shape of `sandbox.env` for good — the one decision with a
long-term cost — and presents the two ways to get there. Nothing here gates
`v0.1.0`.

## Today

`sandbox.env` is `map[string]string` (`input/v1beta1/input.go`), literal
and documented as non-secret; `internal/sandbox.Validate` checks keys and
NUL-free values, admission refuses it without a policy `setEnv` permit,
`engine.RunOptions.Env` becomes `WasiConfig.SetEnv` in
`internal/engine/sandbox.go` (`configureSandbox`, sorted keys); no example
or scaffold template uses `sandbox` yet. `sandbox.filesystem.privateTmp` is
an `os.MkdirTemp` under `$TMPDIR` (`privateTmp`, pre-opened read-write at
`/tmp`, removed on every path out of `Run`) and starts empty. Step
credentials arrive in `RunFunctionRequest.credentials` (name →
`CredentialData{data: map<string, bytes>}`); `cmd/function/fn.go` reads the
pull credential with `request.GetCredentials` and withholds it from the
forwarded request (`withoutCredential`), every other credential is
forwarded whole. `internal/testwasm` has raw-WASI fixtures (`Environ`,
`ReadFile`) that hand the guest's environ or a file back as a `Result`.

## Goals, non-goals, invariants

Goals: an SDK-based guest authenticates without module code touching
credentials; TinyGo and Rust guests are equal (WASI environ and files, no
new import); a Composition states exactly which variables and files exist;
values are never logged, never persisted beyond the run, never read from
the pod; the schema takes new value sources later without a break.
Non-goals: reading anything from the host filesystem or the runtime's
environment; templating values (an INI file from two keys is a `content`
literal or a guest concern); values chosen by the composite resource
(deferred, see Phasing).

1. **Only from the request or the Composition.** A value is a literal in
   the Input or a key of a step credential in the request — never a file,
   socket or environment variable of the runtime.
2. **The XR widens nothing.** Every field below is a `sandbox` field, read
   from the Input only, like every grant.
3. **The pull credential stays withheld.** A credential named by
   `module.oci.credentials` is never a source: what the guest may not see in
   its request it may not see in its environ or `/tmp` either.
4. **A module never sees another module's data.** Values live in one run's
   store and one run's private `/tmp`, removed with it.
5. **Nothing new to grant at the operator level.** Delivering a credential
   the guest already receives into that guest's environ or `/tmp` widens no
   boundary; the existing ceilings apply.
6. **Values are never logged.** Fatal results and log lines name variables,
   paths, credential names and keys — never a byte of a value.

## Shape

Two options were weighed for `env`; the recommendation is the first.

**Option A (recommended): retype `env` to the Pod idiom.** `sandbox.env`
becomes a list of `{name, value | valueFrom}` — exactly a Pod's `env[]` —
with `envFrom[]` for bulk import and `filesystem.files[]` mirroring the
same `valueFrom`:

```yaml
sandbox:
  env:
  - {name: AWS_REGION, value: eu-central-1}          # literal, non-secret
  - name: AWS_ACCESS_KEY_ID
    valueFrom: {credential: {name: aws, key: access_key_id}}
  - name: AWS_SECRET_ACCESS_KEY
    valueFrom: {credential: {name: aws, key: secret_access_key}}
  envFrom:                                           # bulk: every key of a credential
  - credential: {name: vault}
    prefix: VAULT_                                   # VAULT_TOKEN, VAULT_ADDR, …
  filesystem:
    privateTmp: true                                 # required by files
    files:
    - path: /tmp/gcp.json
      valueFrom: {credential: {name: gcp, key: credentials.json}}
    - path: /tmp/ca.pem
      content: "-----BEGIN CERTIFICATE-----\n…"     # a literal — non-secret, like value
```

```go
type Sandbox struct {
    …
    Env     []EnvVar        `json:"env,omitempty"`      // exactly these; a name set twice is refused
    EnvFrom []EnvFromSource `json:"envFrom,omitempty"`  // every key of a source, optionally prefixed
}
// +kubebuilder:validation:XValidation:rule="has(self.value) != has(self.valueFrom)",message="exactly one of value and valueFrom must be set"
type EnvVar struct {
    Name      string       `json:"name"`                // Pattern ^[A-Za-z_][A-Za-z0-9_]*$
    Value     *string      `json:"value,omitempty"`     // +optional
    ValueFrom *ValueSource `json:"valueFrom,omitempty"` // +optional
}
// ValueSource is where a value comes from; exactly one member is set (CEL).
// New kinds (composite, context) are added as members — never a break.
type ValueSource struct {
    Credential *CredentialKeyRef `json:"credential,omitempty"` // {name, key}, both MinLength=1
}
type EnvFromSource struct {
    Credential *CredentialRef `json:"credential,omitempty"` // {name}; exactly one member (CEL)
    Prefix     string         `json:"prefix,omitempty"`     // Pattern as Name
}
type SandboxFilesystem struct {
    PrivateTmp bool          `json:"privateTmp,omitempty"`
    Files      []SandboxFile `json:"files,omitempty"`      // requires PrivateTmp
}
// +kubebuilder:validation:XValidation:rule="has(self.content) != has(self.valueFrom)",message="exactly one of content and valueFrom must be set"
type SandboxFile struct {
    Path      string       `json:"path"`                // Pattern ^/tmp/.+
    Content   *string      `json:"content,omitempty"`   // +optional
    ValueFrom *ValueSource `json:"valueFrom,omitempty"` // +optional
}
```

Why: it is the shape every Kubernetes user already writes (a Pod's
`env[].valueFrom.secretKeyRef`, `envFrom[].secretRef`, and the operator
writes it for this very function in a `DeploymentRuntimeConfig`); one place
defines the environment, one duplicate rule; a new source is one more
optional member of `ValueSource`, shared by variables and files; the map
had no room for a source at all. The cost is a break of an unreleased
`v1beta1` field, which the project accepts before `v1.0.0`; the migration
is mechanical (below) and touches no example or template.

**Option B (fallback): keep the map, add an additive sibling.** `env` stays
`map[string]string`; a new `envFrom: []EnvFromSource{credential: {name,
prefix, keys}}` imports a credential's keys — all of them, or the ones
`keys` selects and renames (`keys: {AWS_ACCESS_KEY_ID: access_key_id}`) —
and `files[]` is as above. It never breaks and keeps literals compact, but
it bends the Kubernetes meaning of `envFrom` (bulk import) into a
per-variable mapping, splits the environment across two fields, and a
future single-value source (a composite field feeding `AWS_REGION`) has no
natural entry in a bulk-import list. Recommendation: A; B only if the map
must stay.

**Migration (option A).** An Input with the map form fails to decode into
the list; rather than a JSON type error, `RunFunction` checks the raw
`structpb` Input first (`sandbox.env` is an object → fatal): `sandbox.env
is a map ({KEY: value}); it is a list of {name, value | valueFrom} entries —
write {name: LOG_LEVEL, value: debug}`. README Input reference, AGENTS,
`docs/abi.md`'s Sandbox row, the sandbox and trust-model one-pagers and the
`internal/sandbox`/`cmd/function` tests change with it; `configureSandbox`
does not (it takes a resolved map). No `v1beta2`: the API is unreleased.

**Sources.** A value comes from a literal (`value`, `content` — the
Composition's, non-secret by construction) or a **step credential** by name
and key, read from `RunFunctionRequest.credentials[name].credential_data
.data[key]` — the Secret's data keys as Crossplane loaded them, bytes
verbatim (a trailing newline in a Secret is delivered as is, like a Pod's
`secretKeyRef`). Foreseen as further `ValueSource` members and deliberately
deferred: a field of the observed **composite** (`spec.`/`status.`, read
like `module.from` through `fieldpath.Pave`) and a **context** key
(`RunFunctionRequest.context`, where earlier steps and environment configs
put values). Both are readable by the guest already; they only serve SDKs
that read variables (a per-tenant `AWS_REGION`), and a composite value is
XR-author-controlled data landing in a variable the Composition named, so
the Composition author would decide which variables the XR may feed (never
one an SDK reads as an endpoint or a credentials URI).

**Policy.** `policy.credentialsAllowList` governs *spending* a credential
on a pull — sending it to a registry host the XR author might choose. A
delivered credential goes nowhere: it is copied from the request the guest
already receives into that guest's environ or `/tmp`. So an XR-chosen
module (`module.from`) may receive these values like a static one — it
already sees every forwarded credential — and no policy field is added. The
one refusal is invariant 3 (`sandbox.env[1].valueFrom.credential names
"registry", the credential that pulls the module: it is never handed to the
guest`), checked after `FromComposite` settled which credential pulls.

**Ceilings and enablement.** None new. `valueFrom` and `envFrom` are enabled by
the policy's `setEnv` (the existing `sandbox.env is refused: the operator policy
(--sandbox-policy-file) does not permit it for this request`, and `sandbox.envFrom
is refused: …`); `files` under the private `/tmp` (policy `usePrivateTmp`) and needs the grant
(`sandbox.filesystem.files requires sandbox.filesystem.privateTmp: the
private /tmp is the only directory a module is given`). Rationale: a module
with a private `/tmp` and the request in hand can already write any
credential to `$TMPDIR` itself, and one with `env` and the request can
already read them — the runtime doing it before the run adds convenience,
not reach; `TMPDIR` on a tmpfs `emptyDir` is what keeps secrets off a disk,
unchanged. Two constants bound the work: `sandbox.MaxFiles` (32) and
`sandbox.MaxFileBytes` (1 MiB per run, all files together — a CA bundle is
~200 KB, a service-account JSON ~3 KB; the request itself is capped at 4 MB
by `--max-recv-message-size`).

**Refusals** — fatal results naming the field, the entry and what to change,
before the module is resolved (shape, flags) or before it runs (lookups):

| condition | fatal result |
|---|---|
| a name not an identifier — `env[].name`, an imported key, or after `prefix` | `sandbox.envFrom[0]: credential "aws" has key ".dockerconfigjson", which is not an environment variable name; name the keys you need in sandbox.env instead` |
| a name set twice (across `env` and every `envFrom` entry) | `sandbox.envFrom[0]: AWS_REGION is already set by sandbox.env[0]` |
| credential absent from the request | `sandbox.env[1].valueFrom.credential: the request carries no credential "aws"; declare it on the pipeline step` |
| key absent | `sandbox.filesystem.files[0].valueFrom.credential: credential "gcp" has no key "credentials.json"` (keys are not listed — an XR condition is readable by the XR author) |
| pull credential named | invariant 3, above |
| NUL byte in a value | `sandbox.env[2]: the value of AWS_SECRET_ACCESS_KEY contains a NUL byte, which WASI cannot pass` |
| `files` without `privateTmp`; a path not under `/tmp/`, not normalized or set twice; over the caps | `sandbox.filesystem.files[1].path "/etc/ssl/ca.pem" must be under /tmp: the private /tmp is the only directory a module is given`; `… "/tmp/../x" must be normalized`; `sandbox.filesystem.files: 1200000 bytes exceed the 1048576-byte cap` |
| grant no policy enables | the existing `--sandbox-policy-file` refusal messages |

## Mechanics

`RunFunction` (`cmd/function/fn.go`) settles the grant where it settles
every grant: `sandbox.Validate` (shape, duplicates, paths, literal sizes),
`Ceiling.Grant` (flags). Values are resolved right after `FromComposite`
and `registryAuth` — the first point where the pull credential's name is
known — by a new `sandbox.Materialize(grant, sandbox.Sources{Credentials:
req.GetCredentials(), Withheld: pullName})` returning `Grant{PrivateTmp,
Env map[string]string, Files []engine.File}` or one of the errors above;
then `withoutCredential`, resolve, verify, load, run as today. The engine
stays ignorant of sources: `RunOptions.Env` already is a resolved map, so
`configureSandbox` does not change; `RunOptions.Files []File{Path, Data}`
is new, written by `privateTmp` after `MkdirTemp` and before `PreopenDir`
(which opens the directory at config time): `os.MkdirAll` for parents
(0700), `os.WriteFile` (0600 — owner-only, which satisfies every SDK that
checks credential-file permissions), and `removePrivateTmp` removes the
whole directory on every path out of `Run`, as now. `path` is a guest path;
the host path is `tmpDir + path[len("/tmp"):]`, and the normalization check
rules out an escape before wasmtime's pre-open resolution ever sees one. No
cache is involved: everything is per run. Cost: one `WriteFile` per file.

The guest sees WASI alone (`docs/abi.md`, Sandbox table gains two rows):
the variables through `environ_get` (`os.Getenv`, `std::env::var`), the
files under the pre-opened `/tmp` (`os.ReadFile`, `std::fs::read`). A Go
guest hands `wasmfn.HTTPClient()` to the SDK (`config.WithHTTPClient` for
AWS SDK v2, `option.WithHTTPClient` for Google) and the SDK's default chain
finds `AWS_*` or `GOOGLE_APPLICATION_CREDENTIALS=/tmp/gcp.json` on its own;
the token exchange (`oauth2.googleapis.com`, `sts.amazonaws.com`) needs its
own `sandbox.egress.http` rule — the grant model is unchanged, and the
metadata endpoint an SDK might probe first is on the default block list.

Audit: one debug line per run (`Sandbox values delivered`: variable names,
file paths and sizes, source kinds), never values; refusals go through
`fatal` (`requests_total{outcome=refused}`); no new metric. Tests:
`internal/sandbox` table tests for every refusal above and the migration
message; `internal/engine` with `testwasm.Fixed(t, rsp, Options{Body:
testwasm.ReadFile("gcp.json")})` reading a materialized file on fd 3 and
`testwasm.Environ()` returning the environ; `cmd/function/fn_test.go` end to
end with `req.Credentials` (delivered, missing credential, missing key, the
pull credential refused, `envFrom` colliding with `env`); a Go guest under
`internal/testwasm/testdata` reading `os.Getenv` and `os.ReadFile("/tmp/x")`
if a real-toolchain check is wanted.

## Trust and threats

The exposure is the request's: the Composition author already hands the
step's credentials to the module; this delivers a copy where an SDK looks.
What changes is *where* a secret sits during a run — the guest's environ
(readable by anything the module logs, e.g. an SDK dumping its environment
at debug level) and a file on the runtime's `$TMPDIR` (a tmpfs `emptyDir`
keeps it off disk; the file exists only between `MkdirTemp` and
`RemoveAll`) — not who can obtain it. With egress granted the module can
still send only to the hosts its Composition listed; `--cosign-key` remains
strongly recommended wherever credentials meet egress. The private `/tmp`
is per run, so no other module or run reads a delivered file; nothing is
cached or persisted. The pull credential is withheld everywhere (invariant
3), so a private-registry secret never reaches a guest through this door
either. An XR author cannot add a variable or file (Input-only fields) and
today cannot influence a value at all. The `env` doc rule becomes: *a
literal `value` is non-secret; `valueFrom` is a credential the step already
carries.*

## Phasing

1. **Shape:** `env` retyped (or `envFrom` added, option B), `envFrom`,
   `filesystem.files`, `ValueSource` in `input/v1beta1`; `sandbox.Validate`;
   the migration fatal; CRD regeneration; README rows — with a "not
   implemented yet" refusal for `valueFrom`/`files` if behaviour trails (the
   pattern the sandbox used). S. Best done in the first release that touches
   `sandbox` after `v0.1.0`, so the map form lives as short as possible.
2. **Behaviour:** `sandbox.Materialize`, `RunOptions.Files`, the fixtures,
   the fatal results above. S+S (env, files).
3. **Deferred, additive:** `composite` and `context` members of
   `ValueSource`; a directory form of `files` (`valueFrom: {credential:
   {name}}` without `key`, one file per key under `path`, the Pod
   secret-volume idiom — refused today, so it can gain that meaning later);
   per-Input caps if the constants prove wrong.

Nothing here gates `v0.1.0`.

## Open questions

- **Option A or B?** Recommendation: A — the Pod idiom, one place, one rule,
  additive sources forever; the map form has no room to grow and the break
  is cheap now. B if the map must stay.
- **Bulk import strictness:** an `envFrom` import that meets a key which is
  not a valid variable name (`.dockerconfigjson`) — refuse the run naming
  the key, or skip it as a Pod does (silently, with an event nobody reads
  in a function)? Recommendation: refuse; `env[].valueFrom` selects.
- **Own action for `files`?** A separate policy action would let an operator
  allow a private `/tmp` yet forbid the runtime writing request bytes into
  it. Recommendation: no — the guest can write the same bytes itself, so
  the action would only forbid the convenient path; keep `files` under the
  private `/tmp`'s `usePrivateTmp` grant, revisit if an operator asks.
- **Composite and context sources now or later?** Recommendation: later
  (Phasing 3) — a module can read both from its request, the SDK-in-env
  argument is weak for non-secret values, and the XR-fed-variable trust note
  wants a real use case behind it.
