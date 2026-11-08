// The operator grant policy of the local render (--sandbox-policy-file): the
// module may unpack into a private /tmp, GET the fixture server's bundles,
// and the runtime may dial loopback to reach it, which its egress block list
// refuses by default. In a cluster the operator grants the real bundle host
// instead, and backs the private /tmp with a tmpfs (TMPDIR on an emptyDir
// with a sizeLimit) to cap what one run may write:
//
//   permit (principal, action == Action::"usePrivateTmp", resource);
//
//   permit (principal, action == Action::"grantEgress", resource == HostPattern::"artifacts.example.com")
//   when { context.method == "GET" };
permit (principal, action == Action::"usePrivateTmp", resource);

permit (principal, action == Action::"grantEgress", resource == HostPattern::"127.0.0.1")
when { context.method == "GET" };

permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isLoopback() };
