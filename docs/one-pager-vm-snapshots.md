# Per-image microVM snapshots

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: draft (an alpha feature in 0.1 behind `--enable-vm-snapshots`, off by default; #279 has what's left)

`function-oci` boots a microVM for each request
([OCI runtime](one-pager-oci-runtime.md)), and the boot is most of a short
run: the kernel start, the mounts and the vsock connect come before the
process does anything. With `--enable-vm-snapshots` the runtime boots an image's
VM once and freezes it where the init waits for a run. Each later request
gets a fresh VM restored from that snapshot. Fresh instance per request
still holds: a clone runs one process, and the runtime kills the clone with
the run.

The feature is Firecracker's alone, since libkrun has no snapshots.

## Design

### Taking a snapshot

The first run of an image at a VM size boots the VM as before, except that
`firecracker` starts with `--api-sock` beside its configuration file. The
init mounts the root filesystem, connects to the host over vsock and says
hello. The host answers `Park` in place of the run. The init answers
`Parked` and blocks reading that connection, with nothing in flight. The
host then pauses the VM (`PATCH /vm`) and has Firecracker write a state
file and a memory file (`PUT /snapshot/create`). It kills the VM once the
files exist, before the VM runs a process or receives a request, an
environment binding or a credential. A snapshot contains the booted kernel,
the init and the mounted root filesystem.

Firecracker writes each page of the VM's memory, and most pages of a
booted guest are zero. The engine rewrites the memory file sparse: it skips
each 64 KiB block of zeros, and a hole reads back as zeros when the restore
maps the file.

### Restoring

A later run of the same image and VM size starts a fresh `firecracker` with
`--api-sock` and loads the snapshot (`PUT /snapshot/load`). The load sets
the run's own vsock socket (`vsock_override`), maps the memory file
privately and resumes the VM. Firecracker resets the guest's vsock
connections across a snapshot: it queues the reset when it writes the
snapshot, and the guest's driver handles it on resume. That reset ends the
read the parked init blocks in. The init connects to the clone's host and
says hello as it does after a boot, and the host sends the run. From there
the run uses the booted run's code, with its frames and its fatal wording.

When the host can't make a snapshot, it logs a warning and boots the VM for
that run. A boot with the flag off still starts `firecracker` with
`--no-api`. Firecracker takes and loads a snapshot through its API alone,
and these two paths open the socket for it. The socket is in the run's
private directory, and the release binary keeps its seccomp filters.

### Warm-up and first runs

Warm-up takes the snapshot of each `--warm-modules` image before the pod
reports ready, for the VM a step with no `limits.memory` of its own gets.
The first request of a warmed image then restores. An image the warm-up
didn't load gets its snapshot at its first run, and a step with its own
`limits.memory` gets one at the first run with that limit.

A run doesn't wait for a snapshot another run is making, because making
one takes longer than a boot: 1.5 s against 1.1 s in the measurements. A
run that finds the snapshot in the making boots, as it would with the flag
off, and no request waits past its deadline for the snapshot of another.

A VM reserves its whole memory from `--max-total-run-memory` before a
restore as before a boot. A restored `firecracker` resided at 20.6 MiB
after the echo run, but the guest may use its whole memory, and the pool
counts that ceiling.

### Freshness of a clone

A clone's memory is a private copy-on-write mapping of the memory file. The
overlay's upper layer and `/tmp` are tmpfs inside that memory, empty at the
snapshot. A clone's writes stay in the clone, and three clones in a row
each found the files of the one before absent.

The wall clock and the kernel's random state come from the snapshot. For a
restored run the description includes the host's time and 32 random bytes.
A parked init sets the clock from the time and reseeds the kernel with the
bytes before it starts the process.

- On arm64 a clone's wall clock continues from the snapshot's moment. With
  the refresh off, a clone restored five seconds after its snapshot read
  five seconds behind the host. With it on, the clone read within a second
  of the host.
- With the seed off, five clones still drew five different values from
  `/dev/urandom`, most likely through the VMGenID device, which Firecracker
  updates on a restore. The seed covers a guest kernel without that device.

Clones of one snapshot share the kernel's address layout, the kernel
secrets from before the park, the monotonic clock's starting value and the
sequence of process IDs. Those clones all run one module, which limits the
exposure to a module's view of its own earlier runs.

### The key

A snapshot restores only into the memory size and the vCPU count it came
from. A step's `limits.memory` sizes the VM: the VM gets that limit plus
64 MiB for the kernel. That makes the key an image, a VM memory size and a
vCPU count. The vCPU count is the operator's `--vm-vcpus`, one value for
the runtime, which leaves `limits.memory` as what multiplies an image's
snapshots.

### In the compiled store

The runtime keeps a snapshot beside the image's root filesystem in the
compiled store, `compiled/<version>/`. It's a state file and a memory file.
Their names start with the entry name of the root filesystem and go on
with the VM's memory size, its vCPU count and a fingerprint of the
Firecracker binary, the guest kernel, the init and the boot arguments. A
different Firecracker, kernel or init looks under another name.

Firecracker writes both files in place, since a VM's memory is too large to
hand over as bytes. The store signs a file under its per-process key once
the file is complete
([Compiled-artifact cache key](one-pager-cache-key.md)). An unfinished file
is no entry, and the runtime removes what an interrupted write left when it
starts. The sweep bounds the snapshots with the store's other entries, and
the `cache_bytes` gauge counts them. Both count a sparse file by what the
disk allocated for it.

A loaded image remembers its snapshots while it stays in memory. When the
runtime loads the image again, the engine looks the snapshot up in the
store, which verifies the signature over the whole file as it does for the
root filesystem. The
engine makes a snapshot again when the store refuses it or the sweep
removed it from under a loaded image. When the engine can't make or
restore a snapshot, that run boots, and the image goes without snapshots
until the runtime loads it again.

A compiled image first runs from a scratch file. Once the runtime has
stored the root filesystem, the engine continues from the stored file
(`Engine::stored`), because a snapshot records the path of the drive it
mounted and a stored file keeps its path. Each run marks the stored root
filesystem and the snapshot it restores as used, which keeps the files of
a busy image out of the sweep. An image the runtime didn't store keeps its
snapshots in the engine's scratch directory for the life of the process.

### The protocol

The protocol between the init and the host is version 2. `Park` (host to
init, in place of `Run`) and `Parked` (the answer) are new frame kinds. The
description gains `now_unix_ns` and `seed`, both empty for a booted run.
The init and the engine come from one image, and an engine that meets
another version refuses the run and reports both versions.

## Measurements

The numbers come from one machine: a laptop's nested KVM (arm64),
Firecracker 1.17.0, the pinned 6.18 guest kernel, one vCPU, the default
512 MiB limit and with it a 576 MiB VM, the echo fixture, 30 runs for each
path after one warm-up run.

| | boot | restore |
|---|---|---|
| the whole run, median | 1123 ms | 250 ms |
| the fastest and the slowest run | 1112 and 1152 ms | 241 and 256 ms |
| `firecracker` starts, and loads the snapshot | 0.2 ms | 3.1 ms |
| until the init connects | 1030 ms | 36 ms |
| the protocol and the process | 85 ms | 206 ms |
| `firecracker`'s resident memory at the exit | 49.7 MiB | 20.6 MiB |
| `firecracker`'s minor page faults | 12 203 | 1 086 |

The first run of an image takes 1.75 s with snapshots on: the boot, then
about 0.6 s to write the memory file and rewrite it sparse. That file is
576 MiB long and 44 MiB on disk, and the state file is 6 KiB. The second
request of an image saves more than the snapshot cost. With the snapshot
taken at warm-up, the first run of the image took 252 ms.

A clone runs its process slower than a booted VM does, because each page it
touches faults in from the memory file. Nested virtualization makes a fault
and a boot expensive: a boot on this machine takes four times the 270 ms a
CI runner measured. The ratio between the two paths transfers better than
the milliseconds do. Nothing ran on x86 or on a GitHub runner yet. The VM
suite prints both paths' times, and `boot_against_restore` in
`crates/engine/tests/vm.rs` is the benchmark, run by hand.

## Decisions

Jonasz settled these on 2026-11-09, with these measurements:

- The prototype merges behind a flag, off by default, and
  [#279](https://github.com/jonasz-lasut/guestfn/issues/279) continues
  from it.
- Snapshots are in 0.1 as an alpha feature, and the flag is
  `--enable-vm-snapshots`.
- A snapshot belongs to one image. Its key is the image and the VM size,
  since `limits.memory` resizes the VM.
- Restore goes through the release binary's API socket, and the engine
  keeps forking `firecracker`. That answers the question
  [#265](https://github.com/jonasz-lasut/guestfn/issues/265) left to this
  work.

## Alternatives

- **An image-independent snapshot.** One generic VM frozen before it mounts
  a root filesystem, with the drive pointed at the request's squashfs after
  each restore. It trades one memory file for the whole runtime against the
  mounts and two more API calls per request. Nobody measured it. Rejected
  (Jonasz, 2026-11-09): a snapshot is per image.
- **A parked init that listens.** Firecracker keeps a guest's listening
  sockets across a restore, and the issue's first sketch had the host
  connect to the init. The guest closes its connections when it handles
  the pending reset. A host that connects before that can lose its
  connection, and the host would need a retry. The driver's code suggests
  this, and nothing reproduced it. Waiting on the open connection doesn't
  need a retry and reuses the boot's accept path.
- **A parked init that polls.** The first version closed its connection and
  tried to connect each 2 ms until a clone's host answered. A snapshot can
  catch a connect in flight, and each clone of that snapshot could then
  wait out the 2 s connect timeout.
- **One VM size per image.** The runtime would snapshot at its memory
  ceiling and leave the limiting to the process's cgroup. That gives one
  snapshot per image, and the memory pool would reserve the ceiling for
  each run. A second size costs 44 MiB with sparse files.
- **Firecracker's `vmm` crate in the engine.**
  [#265](https://github.com/jonasz-lasut/guestfn/issues/265) measured it.
  Restore works there and the whole run takes as long. The crate has no
  seccomp filters, and a VMM in the runtime's address space shares the
  runtime's fate. The release binary's API answers a load about 3 ms after
  the process starts.
- **A `userfaultfd` handler.** Firecracker can hand a clone's page faults
  to a handler, which could load the recorded working set at once. The
  measured gain doesn't need it.
  [#279](https://github.com/jonasz-lasut/guestfn/issues/279) reopens it if
  a runner's numbers show that the clone's slower process phase matters.

## What's left

[#279](https://github.com/jonasz-lasut/guestfn/issues/279) tracks it:

- the numbers from a GitHub runner, the first x86 run among them
- restores under load, which the measurements ran one at a time
- whether the flag is on by default once the feature leaves alpha
