// The operator grant policy of the local render (--sandbox-policy-file): the
// module may GET team records from the fixture server, and the runtime may
// dial loopback to reach it, which its egress block list refuses by default.
// In a cluster the operator grants the real CMDB host instead:
//
//   permit (principal, action == Action::"grantEgress", resource == HostPattern::"cmdb.example.org")
//   when { context.method == "GET" && context.path like "/teams/*" };
permit (principal, action == Action::"grantEgress", resource == HostPattern::"127.0.0.1")
when { context.method == "GET" && context.path like "/teams/*" };

permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isLoopback() };
