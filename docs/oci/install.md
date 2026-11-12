# Install function-oci

This page is for operators installing `function-oci`, the runtime that runs
an OCI image as a composition function step
([design record](../one-pager-oci-runtime.md)). It covers what the package
needs from a node, its flags, and what it refuses. The Input, the policy
layers, the caches and the signatures work as they do for function-wasm:
the pages under Operators describe both packages unless they say
otherwise. The package is alpha, and
[Compatibility](../contract/compatibility.md#function-oci-at-0x) lists what
may change between its releases.

## One microVM per request

`function-oci` boots one microVM per request with Firecracker. The VM gets
its own guest kernel, the image's root filesystem read-only under a tmpfs
overlay, the step's `limits.memory` for the process (at least 16 MiB,
held by a cgroup the init puts the process in, so the kernel's
out-of-memory killer ends a process past it at once) plus 64 MiB for the
kernel, the init, the overlay and the page cache of the root filesystem,
and no network device. Inside it, the runtime's init starts the image's
configured command as the image's configured user, the host calls the
function over gRPC, and the init relays the process's output and exit
status to the host ([the contract](modules.md)). The
VM ends with the run, and the next request boots a fresh one.

The runtime unpacks an image once per digest into a squashfs root
filesystem, which the compiled store keeps beside function-wasm's
artifacts, under the same key and the same LRU sweep. The module cache's
`compile` metrics time the unpacking.

## Node requirements

The pod needs `/dev/kvm`. An unprivileged container opens only the
devices its container runtime admitted into its device cgroup, and a
`hostPath` mount of the device node doesn't admit the device: the node appears
in the container and every `open` of it fails with `Operation not permitted`.
Kubernetes admits a device through a
[device plugin](https://kubernetes.io/docs/concepts/extend-kubernetes/compute-storage-net/device-plugins/):
the plugin advertises the node's `/dev/kvm` as a resource and the kubelet admits
the device into the container of a pod that requests one. A plugin that
advertises any device path is
[generic-device-plugin](https://github.com/squat/generic-device-plugin). Apply
its
[manifest](https://github.com/squat/generic-device-plugin/blob/main/manifests/generic-device-plugin.yaml)
with the container's `--device` arguments replaced by this one:

```yaml
args:
- --device
- |
  name: kvm
  groups:
  - count: 64
    paths:
    - path: /dev/kvm
```

`count` is how many pods may hold the device at once, since every one of
them shares the one `/dev/kvm`.

The kubelet gives the container the node's device node unchanged, owner
and mode included, `root:kvm 0660` on most node images, and the
container runs as the package's non-root user, who isn't in that group.
A `DeploymentRuntimeConfig` requests the device and puts the node's `kvm`
group among the pod's `supplementalGroups`:

```yaml
apiVersion: pkg.crossplane.io/v1beta1
kind: DeploymentRuntimeConfig
metadata:
  name: function-oci
spec:
  deploymentTemplate:
    spec:
      selector: {}
      strategy: {}
      template:
        spec:
          securityContext:
            supplementalGroups: [994]
          containers:
          - name: package-runtime
            resources:
              limits:
                devic.es/kvm: "1"
```

Point the Function at it:

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-oci
spec:
  package: ghcr.io/jonasz-lasut/function-oci:v<version>
  runtimeConfigRef:
    name: function-oci
```

The gid is the node image's (994 on the end-to-end suite's kind node;
images differ), so read it once per node pool, through the API alone:

```shell
kubectl debug node/<node> -it --image=busybox -- stat -c %g /host/dev/kvm
```

Delete the `node-debugger-*` pod `kubectl debug` leaves behind.

The pod doesn't need `privileged` or any capability. A privileged
DaemonSet with the node's `/dev` mounted can set the device to `0666` in
an init container instead, which is what KubeVirt does. A `privileged: true` function container gets every
device of the node admitted, at the cost of the boundary the pod keeps
otherwise. The end-to-end suite's setup
runs the plugin and patches the kind node's gid into the `oci` profile's
runtime config (`crates/e2e/cluster/kvm-device-plugin.yaml`,
`crates/e2e/profiles/oci/default/`). The runtime refuses to
start without the device and says so:

```text
cannot open /dev/kvm: No such file or directory; the OCI runtime boots a microVM per request and needs the device granted to its pod (docs/oci/install.md)
```

`Operation not permitted` in place of `No such file or directory` is the
`hostPath` mount: the node is there, the device isn't admitted.
`Permission denied` is the mode: the kubelet admitted the device, and
the pod's user is neither its owner nor in its group.

A VM's memory comes out of the pod's. The container's memory limit
needs `--module-memory-limit` plus 64 MiB for each of
`--max-concurrent-runs`, and the runtime's own use on top.
`--max-total-run-memory` bounds the VMs together. A VM reserves its whole
memory before it boots.

## Flags

`function-oci` takes `function-wasm`'s flags
([Runtime flags](../operators/runtime-flags.md)) with the same defaults, and these:

| flag | env | default | what |
|---|---|---|---|
| `--vmm-binary-path` | `VMM_BINARY_PATH` | `/usr/lib/guestfn/firecracker` | the Firecracker binary |
| `--guest-kernel-path` | `GUEST_KERNEL_PATH` | `/usr/lib/guestfn/vmlinux` | the guest kernel, an uncompressed `vmlinux` |
| `--guest-init-path` | `GUEST_INIT_PATH` | `/usr/lib/guestfn/init` | the init the runtime boots as the VM's first process |
| `--vm-vcpus` | `VM_VCPUS` | `1` | vCPUs per VM |
| `--enable-vm-snapshots` | `ENABLE_VM_SNAPSHOTS` | `false` | restore a snapshot of the booted VM per image in place of a boot. Warm-up takes the snapshot of a `--warm-modules` image, and an image's first run takes it otherwise. An alpha feature: the [design record](../one-pager-vm-snapshots.md) has what it does and what it measured |

The package image includes the three files. `--module-stack-limit` has no
meaning for a process and the runtime ignores it. `--module-timeout` and a
step's `limits.timeout` bound the whole run, the boot included.

## Refusals

- A WebAssembly artifact: `module oci … is a WebAssembly artifact, which
  function-oci does not serve; function-wasm serves it`, or, for a manifest
  that declares one, `… is a WebAssembly module (wasm@1.0) by its manifest,
  which function-oci does not serve; function-wasm serves it`. Install
  `function-wasm` beside `function-oci` and point the step at it.
- An image for another platform: `cannot load module …: image is built for
  linux/arm64, and this runtime runs on linux/amd64`.
- An image without a command: `cannot load module …: image has neither an
  entrypoint nor a command: the contract runs the image's configured command
  (docs/one-pager-oci-runtime.md)`.
- A manifest that requires egress: `module … requires egress
  (requires.egress.http), which function-oci does not serve yet: the VM has
  no network device (docs/one-pager-oci-runtime.md)`. The proxy the design
  record describes is a later phase.
- A layer compressed with zstd: `cannot load module …: layer is compressed
  with zstd, which the OCI runtime does not unpack yet; build the image with
  gzip layers (the default)`.

A process that exits before answering the call, or an error from the
function, is a fatal result on the step, as a component's error is: `module … failed: process exited with status 3 before answering
the call: <the last line it logged>`. A process the init can't start is `module … failed: cannot start the process: <error>`, and one a signal ended is `module … failed: process was killed by signal 9: <the last line it logged>`. The deadline and the memory limit end the run the same
way: `module … failed: run failed: module exceeded its execution deadline
(30s)` and `module … failed: process was killed: the run's memory limit of
512Mi was exceeded`. A VM whose init never reaches the host before the
deadline is the runtime's failure, not the module's: `module … failed: run
failed: the guest did not connect before the deadline: the VM booted but
its init never reached the host`. The VM's serial console, which `--debug`
logs with the run, says how far the boot got.

## Try it in a kind cluster

From a checkout of the repository, on a machine with `/dev/kvm`:

```shell
git submodule update --init build
make e2e.up E2E_WASM=0 E2E_OCI=1
```

It brings up a kind cluster with Crossplane, the device plugin, a registry
and `function-oci` built from the checkout, installed with
`make local.xpkg.deploy.function.function-oci` on the runtime config
`e2e-oci-default`. `make e2e.down` removes the cluster.

On a Mac, Colima with nested virtualization
(`colima start --vm-type vz --nested-virtualization`) gives the kind node
the VM's `/dev/kvm`, at about 5 s per run. function-oci 0.1 runs on KVM
only, and Docker Desktop doesn't expose it.

## The development loop

`crossplane render` works with `function-oci` as it does with
`function-wasm` ([Render locally](../wasm/render-locally.md)), on a Linux
machine with `/dev/kvm`: run the runtime with `--insecure` and the three VM
flags pointing at a Firecracker binary, a guest kernel and the init that
`make oci.tools` fetches and `make oci.init` builds. The loop needs a
registry, because an OCI module comes from one alone (`module.type: OCI`:
the runtime refuses a `Path` or `HTTP` source by name). Run a `registry:3`
container on `127.0.0.1:5000`, then `docker push` the image there and
`guestfn push --image` the module. The runtime speaks plain HTTP to
`localhost`, `127.0.0.1` and `[::1]` alone. A registry in a cluster needs a
certificate the pod trusts, through `SSL_CERT_FILE` on the function's
container, as the end-to-end suite's registry does
(`crates/e2e/profiles/oci/default/drc.yaml`). To try your own module in a
cluster, push it to a registry with a public certificate, such as GHCR.
