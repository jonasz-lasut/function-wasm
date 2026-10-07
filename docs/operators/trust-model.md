# Trust model

The complete model — parties, what pins the code, credentials, what the
guest sees, threats considered — is
[docs/one-pager-trust-model.md](../one-pager-trust-model.md); this is the
short version. How vulnerabilities in the dependencies of the runtime, of
scaffolded projects and of the examples get fixed is in
[SECURITY.md](../project/security.md).

A module runs with the privileges of the Composition that references it: it
sees the request's observed and desired state and context, exactly as a
native function would, but of the step credentials only those it was
granted - each its manifest names (a `requires.env` binding's credential, or
one in `requires.credentials`) and both Cedar layers permit
(`spendCredential`); the runtime edits every other one out of the request
at the wire level. The credential that pulled it (`module.oci.credentials`)
is the host's and never reaches the guest, whatever its manifest asks. A
native function, like the Go runtime before this one, receives every step
credential. With `module.from` the **composite resource's author** picks the
module — use it where XR authors are trusted to, fence what they can pick
with the `compositionPolicy`'s `pullModule` permits (required for `OCI` and
`HTTP` sources: without them the XR author would point the runtime at any
host and read what its answer says), and restrict it to signed code with
`--cosign-key`. A source read from the XR may name a step credential only
where a `spendCredential` permit matches it, and only for a repository a
`pullModule` permit admits: otherwise the XR author would pick the
registry host the secret is sent to, and a registry that answers with a
`Basic` challenge receives it — without such a permit an XR-chosen module is
pulled with the runtime's own Docker config (mount one through a
`DeploymentRuntimeConfig` and set `DOCKER_CONFIG`; credentials there are
bound to their registry host) or anonymously. `compositionPolicy` and
`limits` are read from the Input only, so an XR author can choose code, not
widen its permissions, grants or budget; every one of these rules is
enforced by the runtime on every request (Crossplane never installs the
Input CRD), and [`function validate`](../guests/validate.md) runs the
same code over a Composition offline. The sandbox protects the runtime
process — and with it every other Composition sharing the Function — from a
crashing, looping or memory-hungry module, and gives a module no filesystem,
environment or network beyond what its manifest requires and both Cedar
layers (the Input's `compositionPolicy` and the operator's
`--sandbox-policy-file`) permit: a private `/tmp` that exists for one
request (host directories are never mountable — the request is a module's
only view of the world beyond what it writes for itself), exactly the
environment variables its manifest binds to step credentials (non-secret
configuration travels in `config`), and HTTP requests through the host to
the hosts, methods and paths its manifest lists within both policies
(block list, budgets). A module granted egress can send whatever its
request carries - the step credentials it was granted included - to those
hosts, which is why
the grant is the policy layers' alone (a manifest can only ask, and an XR
author widens nothing), every request leaves an audit line with the module
digest, and `--cosign-key` is strongly recommended wherever egress is
granted. Every remote module is pinned by a digest the Composition states —
the OCI reference's manifest digest (the manifest names the layer's digest,
and both are verified on fetch) or `http.digest` — so nothing that runs can
change without the Composition changing.
