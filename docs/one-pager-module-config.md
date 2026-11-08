# Module configuration from the composite resource

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.0 (phases 2 to 4, #262; the open questions settled: objects alone, `guestfn scaffold xrd` later)

The Input's `config` block is today a value the Composition author writes
and the runtime forwards to the module verbatim, checked against the module
manifest's `config.schema`. That design answers one question, how the author
configures a module they picked, and it stops making sense the moment the
module is code the Composition didn't pick. With `module.from` the composite
resource chooses the module, the author can't know its schema, `guestfn
scaffold composition` has no skeleton to write, and the schema check doesn't
check anything. The tenant who chose the code has no sanctioned way to
configure it, short of a module reading its own fields out of the composite
resource, which the runtime neither checks nor documents. This record
replaces the value with a reference: `config` becomes an object that says
where in the composite resource the module's configuration is, and the
runtime reads it, checks it against the chosen module's schema and hands the
module the result. Static configuration goes. The record targets the
WebAssembly runtime on `main`; the OCI runtime (the [OCI
runtime](one-pager-oci-runtime.md) record, its code in
[#251](https://github.com/jonasz-lasut/guestfn/pull/251)) adopts it
once that code has merged and its process contract has settled. Jonasz decided to drop the
static value on 2026-11-07.

## Who knows what about a configuration

A module's configuration has two authors with different knowledge. The
module's author knows its structure and writes it down as `config.schema`
in the manifest. The person who deploys an instance of the module knows the
values. In a Composition pipeline that person is whoever writes the
composite resource, since the composite resource is the one object per
instance. The Composition author sits between the two and assembles the
pipeline; they know the module's structure only when they also wrote the
module, and the values only when they also own the instance. A value
written in the Composition exists once per Composition, the tenant it
serves can't change it, and in the `module.from` case the author wrote it
against a schema they haven't seen.

Kubernetes already has the two places this information belongs. A
structure goes in the composite resource definition's OpenAPI schema, which
the API server enforces at admission, and defaults go there too, where the
API server applies them. The module manifest's `config.schema` is a JSON
Schema and transplants into that definition unchanged. What the Composition
then has to say is one thing: which field of the composite resource the
module reads. The runtime still checks the value against the manifest's
schema, as the backstop, since a definition without the schema is valid,
and because the fatal result it writes states the module and the path.

Static configuration had one remaining use, settings the author fixes for
every instance. Those are the definition's defaults, or the module's own.
No code in the project composes a static value with a per-instance one, and
a merge rule between the two would be a third place to look for a setting.

## The field

```yaml
apiVersion: guestfn.crossplane.io/v1
kind: Input
module:
  type: OCI
  from: spec.addOn
compositionPolicy: |
  permit(principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example-org/webapp-addons");
config:
  from: spec.addOnConfig          # the composite resource field with the module's configuration
  # allowEmpty: true              # an unset field runs the module unconfigured
```

`config` is an object with two fields, both read from the Input only:

- `config.from` (required within `config`): a field of the observed
  composite resource under `spec.` or `status.`, with the same grammar and
  the same reader as `module.from`. The field contains the module's
  configuration as the module's `config.schema` describes it, an object in
  every scaffold.
- `config.allowEmpty` (optional, default false): with it, a field the
  composite resource leaves unset (absent or null) runs the module
  unconfigured, the state a module is in today when the Input has no
  `config`. Without it an unset field is a fatal result that states the
  field. The default is strict, as `module.from`'s is: the Composition
  author declared where the module's configuration comes from, and silence
  is a mistake until the author says it isn't.

An Input without `config` runs the module unconfigured, as today. The
runtime refuses any other key under `config` by name, so a 0.6-style value
fails at admission with a sentence that says where the value now belongs.

The object, rather than a string path beside `module`, is what leaves room
for the next field. `allowEmpty` is the first. A future field has the same
home.

## The runtime's steps

`from::from_composite` does the reading at admission, in the same step and
under the same rules as for `module.from`, from the observed composite
resource of the request and before the resolver sees the module. A request
without an observed composite resource, a path the resource doesn't have,
or a value that isn't an object is a fatal result worded as `module.from`'s,
with `config.from` in place of `module.from`:

```text
config.from: cannot read spec.addOnConfig from the composite resource: addOnConfig: no such field
config.from: spec.addOnConfig of the composite resource is not an object
config.from spec.addOnConfig: no observed composite resource to read it from
```

Once the runtime has read the module's manifest (step 6 of `runner.rs`),
it checks the value against `config.schema` exactly as it checked the
static value, and the refusal keeps its wording, `module <desc> config does
not match the module's schema: /greeting: got number, want string`. A
module without a schema accepts any object. An unset field under
`allowEmpty` skips the check, as an absent `config` does today.

The module receives the configuration where every guest already reads it:
`input.config` of its request. The forwarded request is the caller's raw
bytes with wire-level edits (`protowire.rs`), and this is one more. Before
it forwards the request, the runtime replaces the `config` entry of the
Input's Struct, where the caller wrote `{from: …}`, with the resolved
value. The Input is the one field of the request whose schema is the
runtime's own, and since `google.protobuf.Struct` is a map, re-encoding it
keeps every key the caller wrote, known or not. The rest of the request,
the observed composite resource included, passes unchanged, and the proxy
remains wire-level everywhere else. Under `allowEmpty` with an unset field, the
runtime removes the `config` entry, so `GetConfig` reports an unconfigured
module rather than a `from` reference.

The guest glue doesn't change. `GetConfig` reads `input.config`, as it
does on every scaffold today, and a module built before this record runs
unchanged. The world doesn't change either, and the contract remains
`wasm@1.0`, since nothing in the host-guest ABI moved; the group
`guestfn.crossplane.io/v1` takes the new `config` object before any release
includes it.

## Security

Configuration widens nothing. Capabilities come from the three policy
layers and the manifest, credentials from the step's credentials and the
manifest's `requires.env` bindings, never from `config`, and the Cedar
schema doesn't mention it. A tenant who sets `spec.addOnConfig` controls
what a module they could already choose (under `module.from`) or what the
Composition author exposed of a fixed module, within the schema the
module's author published. The runtime checks the value against that
schema, so a module's author can rely on the structure they declared. The
API server's bound on the composite resource bounds the value's size.
Secrets don't belong in configuration. The guests' page repeats this for
tenant-authored configuration, and points at step credentials.

`compositionPolicy` and `limits` remain read from the Input only, as the
[Module source schema](one-pager-module-source-schema.md) record requires.
`config` joins `module.from` as the second thing the composite resource
may author.

## Checks offline

`function validate` applies the rules of `config` to every step and, with
`--xr`, reads `config.from` from that composite resource and prints the
runtime's own refusal where the read fails. With `--resolve` it checks the
value against the resolved module's schema, as it does today for the
static value. The conformance goldens record every refusal:

- `config.from` missing from `config`, or the path outside `spec.`/`status.`
- an unknown key under `config`, stating the key and the two that exist
- `config.allowEmpty` without `config.from` (the object makes this a
  structural error rather than a semantic one, but the sentence remains for
  a reader who wrote the field alone)
- the three read failures of the preceding section, and the schema refusal

## The CLI

`guestfn scaffold composition` writes `config: {from: spec.config}` in the
step and, from the manifest's `config.schema`, a commented definition
fragment beside it: the `spec.config` property with the schema
transplanted, ready to paste into the composite resource definition. A new
`guestfn scaffold xrd` writes a whole definition skeleton for a module,
with the same property and `x-kubernetes-preserve-unknown-fields: true`
where the manifest has no schema. The scaffolds' `example/composition.yaml`
move their greeting to the example composite resource and reference it, so
a freshly scaffolded project renders the way the record says.

## Compatibility

`config` changes from an opaque value to an object of `from` and
`allowEmpty`. The group `guestfn.crossplane.io/v1` has no release behind
it yet, and the runtime refuses the 0.6 group `wasm.fn.crossplane.io` by
name, so no served API changes meaning. The
[Compatibility](contract/compatibility.md) page lists the new field and
its rules among the 1.x promise. The runtime refuses a static value with a
sentence that states `config.from` and never forwards it, so a Composition
written against a pre-release build fails at admission and says what to
write.

## Phases

1. This record.
2. The runtime: `input.rs` (`Config { from, allow_empty }`, the CRD
   regenerated), `from.rs` reads the field, `admission::admit` applies the
   rules, `protowire.rs` replaces the Input's `config` entry, `runner.rs`
   and `validate.rs` share the steps, fixtures under `testdata/validate/`
   and conformance goldens for every sentence, the Input reference, the
   guests' page and troubleshooting updated.
3. `guestfn scaffold composition`'s step and fragment, `guestfn scaffold
   xrd`, the six scaffolds' examples and goldens, the guest suite's
   configured-greeting cases moved to the observed composite resource.
4. The use-case examples (`function-wasm-python`'s `cmdbUrl`, the hello guests'
   greeting) moved to their composite resources and definitions.
5. The OCI runtime, after its code merges: the same `config` object,
   delivered the way its process contract then says (the record is
   weighing a gRPC server in place of standard input, which would make
   this the same wire-level edit).

## Alternatives

- **Keep the static value beside `config.from`, merged.** Fixed settings
  for every instance beside per-instance ones, with the Composition's keys
  taking precedence. Rejected. The definition's defaults, which the API
  server applies, are those fixed settings, and a merge rule would add a
  third place a setting can come from.
- **A `configFrom` string beside `config`.** Additive, so a module whose
  configuration has a `from` key would keep working. Rejected once the
  value goes: nothing collides with the key any more, and the object
  leaves room for `allowEmpty` and the next field.
- **Configuration inside the `module.from` object.** `spec.addOn: {ref: …,
  config: {…}}`, the choice and the settings together. Rejected: the
  `from` objects are the source types, decoded without extra keys, and a
  fixed module with tenant-tunable settings has no `module.from` to put
  them in.
- **Resolution in the guest glue.** `GetConfig` follows the reference
  itself. Rejected: nine glues and every hand-written guest would have to
  agree, and the runtime couldn't check what it never read.
- **Default-lenient, `config.errorOnEmpty: true` to make an unset field
  fatal.** The same switch the other way. Rejected for symmetry with
  `module.allowEmpty`: one word, one default, across the two fields the
  composite resource may author.

## Open questions

Both settled by the owner on 2026-11-08 (Jonasz, #262):

- `config.from` references an object. The scaffolds all declare objects.
  The runtime refuses a scalar or a list with `config.from: <field> of the
  composite resource is not an object`, and the question returns with a
  module whose schema declares otherwise.
- `guestfn scaffold xrd` follows later. The commented definition fragment
  `scaffold composition` writes covers the common case.
