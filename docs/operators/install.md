# Install

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-wasm
spec:
  package: ghcr.io/jonasz-lasut/function-wasm:v0.6.0
```

Also mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`.

The function supports the three Crossplane minor releases Crossplane itself
maintains, today 2.4, 2.3 and 2.2: every release is run through the
end-to-end suite on each of them before it's published ([the release
matrix](../project/testing.md#the-release-matrix)).
