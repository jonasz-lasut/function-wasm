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
`hostPath` mount of the device node admits nothing: the node appears in
the container and every `open` of it fails with `Operation not permitted`.
Kubernetes admits a device through a [device
plugin](https://kubernetes.io/docs/concepts/extend-kubernetes/compute-storage-net/device-plugins/):
the plugin advertises the node's `/dev/kvm` as a resource and the
kubelet admits the device into the container of a pod that requests one.
A plugin that advertises any device path is
[generic-device-plugin](https://github.com/squat/generic-device-plugin);
run it as a DaemonSet with the device named:

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
them shares the one `/dev/kvm`. Then request it through a
`DeploymentRuntimeConfig`:

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
          containers:
          - name: package-runtime
            resources:
              limits:
                devic.es/kvm: "1"
```

The kubelet gives the container the node's device node unchanged, owner
and mode included, `root:kvm 0660` on most node images, and the
container runs as the package's non-root user, who isn't in that group.
Put the node's `kvm` group among the pod's `supplementalGroups`:

```yaml
      template:
        spec:
          securityContext:
            supplementalGroups: [994]
```

The gid is the node image's (994 on the end-to-end suite's kind node;
images differ), so read it once per node pool, through the API alone:

```shell
kubectl debug node/<node> -it --image=busybox -- stat -c %g /host/dev/kvm
```

The pod doesn't need `privileged` or any capability. A privileged
DaemonSet with the node's `/dev` mounted, which the device plugin already
is, can set the device to `0666` in an init container instead, which is
what KubeVirt does. A `privileged: true` function container gets every
device of the node admitted, at the cost of the boundary the pod keeps
otherwise. The end-to-end suite's setup
runs the plugin and patches the kind node's gid into the `oci` profile's
runtime config (`crates/e2e/cluster/kvm-device-plugin.yaml`,
`crates/e2e/profiles/oci/default/`). The runtime refuses to
start without the device and says so:

```text
cannot open /dev/kvm: No such file or directory; the OCI runtime boots a microVM per request and needs the device granted to its pod
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

| flag | default | what |
|---|---|---|
| `--vmm` | `/usr/lib/guestfn/firecracker` | the Firecracker binary |
| `--guest-kernel` | `/usr/lib/guestfn/vmlinux` | the guest kernel, an uncompressed `vmlinux` |
| `--guest-init` | `/usr/lib/guestfn/init` | the init the runtime boots as the VM's first process |
| `--vm-vcpus` | `1` | vCPUs per VM |
| `--enable-vm-snapshots` | off | restore a snapshot of the booted VM per image in place of a boot. Warm-up takes the snapshot of a `--warm-modules` image, and an image's first run takes it otherwise. An alpha feature: the [design record](../one-pager-vm-snapshots.md) has what it does and what it measured |

The package image includes the three files. `--module-stack-limit` has no
meaning for a process and the runtime ignores it. `--module-timeout` and a
step's `limits.timeout` bound the whole run, the boot included.

## Refusals

- A WebAssembly artifact: `module oci … is a WebAssembly module (type
  wasm), which function-oci does not serve; function-wasm serves it`.
  Install `function-wasm` beside `function-oci` and point the step at it.
- An image for another platform: `cannot load module …: image is built for
  linux/arm64, and this runtime runs on linux/amd64`.
- An image without a command: `cannot load module …: image has neither an
  entrypoint nor a command`.
- A manifest that requires egress: `module … requires egress, which
  function-oci does not serve yet`. The VM has no network device, and the
  proxy the design record describes is a later phase.
- A layer compressed with zstd: `cannot load module …: layer is compressed
  with zstd, which the OCI runtime does not unpack yet`. Build the image
  with gzip layers, the default of every builder.

A process that exits before answering the call, or an error from the
function, is a fatal result on the step, as a component's error is: `module … failed: process exited with status 3 before answering
the call: <the last line it logged>`. The deadline and the memory limit end the run the same
way: `module … failed: run failed: module exceeded its execution deadline
(30s)` and `module … failed: process was killed: the run's memory limit of
512Mi was exceeded`. A VM whose init never reaches the host before the
deadline is the runtime's failure, not the module's: `module … failed: run
failed: the guest did not connect before the deadline: the VM booted but
its init never reached the host`. The VM's serial console, which `--debug`
logs with the run, says how far the boot got.

## The development loop

`crossplane render` works with `function-oci` as it does with
`function-wasm` ([Render locally](../wasm/render-locally.md)), on a Linux
machine with `/dev/kvm`: run the runtime with `--insecure` and the three VM
flags pointing at a Firecracker binary, a guest kernel and the init that
`make oci.tools` fetches and `make oci.init` builds. The loop needs a
registry, because an OCI module comes from one alone (`module.type: OCI`:
the runtime refuses a `Path` or `HTTP` source by name). Run a `registry:3`
container on `127.0.0.1:5000`, which the runtime reaches over plain HTTP,
then `docker push` the image there and `guestfn push --image` the module.
A machine without KVM has no loop: function-oci 0.1 runs on KVM only.
