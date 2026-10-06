// Native tests for the guest's logic under dotnet test (MSTest): the fetch
// and log doubles stand in for the world's imports, exactly as the other
// guests' native tests stub their hosts.

using Apiextensions.Fn.Proto.V1;
using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;
using HelloDotnet;

namespace HelloDotnet.Tests;

[TestClass]
public sealed class FunctionTests
{
    static void Log(LogLevel level, string msg, params (string Key, string Value)[] kv)
    {
    }

    static string FakeFetch(string url) => url == "https://greetings.example.com/en"
        ? "howdy"
        : throw new InvalidOperationException($"internal-error: sandbox.egress: no rule admits host \"{url}\"");

    static RunFunctionRequest Request(Struct? config = null, params string[] desired)
    {
        var req = new RunFunctionRequest
        {
            Meta = new RequestMeta { Tag = "hello" },
            Observed = new State
            {
                Composite = new Resource
                {
                    Resource_ = Struct.Parser.ParseJson(
                        """{"apiVersion": "example.org/v1", "kind": "XR", "metadata": {"name": "my-xr"}}"""),
                },
            },
        };
        if (config is not null)
        {
            req.Input = new Struct { Fields = { ["config"] = Value.ForStruct(config) } };
        }
        if (desired.Length > 0)
        {
            req.Desired = new State();
            foreach (var name in desired)
            {
                req.Desired.Resources[name] = new Resource();
            }
        }
        return req;
    }

    static Struct Config(string key, Value value) => new() { Fields = { [key] = value } };

    static string GreetingOf(RunFunctionResponse rsp) =>
        rsp.Desired.Resources["greeting"].Resource_.Fields["data"].StructValue.Fields["greeting"].StringValue;

    [TestMethod]
    public void DefaultGreeting()
    {
        var rsp = Function.RunFunction(Request(), FakeFetch, Log);
        Assert.AreEqual("hello my-xr", GreetingOf(rsp));
        Assert.AreEqual("hello", rsp.Meta.Tag);
        Assert.AreEqual(60, rsp.Meta.Ttl.Seconds);
        Assert.AreEqual("greeted my-xr", rsp.Results[0].Message);
        Assert.AreEqual("FunctionSuccess", rsp.Conditions[0].Type);
    }

    [TestMethod]
    public void ConfiguredGreetingKeepsDesired()
    {
        var rsp = Function.RunFunction(
            Request(Config("greeting", Value.ForString("hi")), "other"), FakeFetch, Log);
        Assert.AreEqual("hi my-xr", GreetingOf(rsp));
        Assert.IsTrue(rsp.Desired.Resources.ContainsKey("other"));
    }

    [TestMethod]
    public void BadConfigIsAnError()
    {
        var e = Assert.ThrowsExactly<RunException>(
            () => Function.RunFunction(Request(Config("greeting", Value.ForNumber(7))), FakeFetch, Log));
        Assert.AreEqual("cannot read config: greeting must be a string", e.Message);
    }

    [TestMethod]
    public void GreetingFromUrlThroughTheFetcher()
    {
        var rsp = Function.RunFunction(
            Request(Config("greetingUrl", Value.ForString("https://greetings.example.com/en"))), FakeFetch, Log);
        Assert.AreEqual("howdy my-xr", GreetingOf(rsp));

        var e = Assert.ThrowsExactly<RunException>(
            () => Function.RunFunction(
                Request(Config("greetingUrl", Value.ForString("https://evil.example.com/en"))), FakeFetch, Log));
        Assert.AreEqual(
            "cannot fetch greeting: internal-error: sandbox.egress: no rule admits host \"https://evil.example.com/en\"",
            e.Message);
    }

    [TestMethod]
    public void HandleRoundTripReportsFatal()
    {
        var req = new RunFunctionRequest { Meta = new RequestMeta { Tag = "t" } };
        var rsp = RunFunctionResponse.Parser.ParseFrom(
            Function.Handle(req.ToByteArray(), FakeFetch, Log));
        Assert.AreEqual("t", rsp.Meta.Tag);
        Assert.AreEqual(Severity.Fatal, rsp.Results[0].Severity);
        Assert.AreEqual("cannot get observed composite resource: none in request", rsp.Results[0].Message);
    }
}
