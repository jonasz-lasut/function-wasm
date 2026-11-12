<Two to four sentences: what this release is, who it matters to, and the one or two changes a reader should know before anything else.>

## Before you upgrade

<What a reader must do, in order, a short paragraph each: what stops working, the exact flag or refusal quoted, and what to change. A deprecation names the release that removes it.>

## For operators

### <The change, named as an operator would name it>

<A paragraph: what the runtime does now and what it did before, why it changed, and what an operator does about it. Quote the flag, field or refusal. Link the docs page that covers it and the pull request (#PR).>

## For module authors

### <The change, named as a module author would name it>

<A paragraph of the same shape for someone writing or building a module (#PR).>

## Security

<For a patch release: each CVE or GHSA, what it affected and whether a reader was exposed. Any finding left unfixed, and why.>

<function-wasm only: the sentence naming the world, wasmtime and WASI versions it serves.>

## Merged pull requests

<Every pull request merged since the previous release of this component, as GitHub lists them, none left out and none reworded:>

* <title> by @<author> in <pull request URL>

**Full changelog**: https://github.com/jonasz-lasut/guestfn/compare/<previous tag>...<new tag>
