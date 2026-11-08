// The operator grant policy of the local render (--sandbox-policy-file): the
// module may GET its greeting from the fixture server render.sh starts, and
// the runtime may dial loopback to reach it, which its egress block list
// refuses by default. In a cluster the operator grants the real host instead.
permit (principal, action == Action::"grantEgress", resource == HostPattern::"127.0.0.1")
when { context.method == "GET" };

permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isLoopback() };
