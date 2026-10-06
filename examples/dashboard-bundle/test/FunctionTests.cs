// Native tests for the function under dotnet test (MSTest): bundles are
// zipped in memory, the fetch is a double serving them, and each run unpacks
// into a temporary directory standing in for the private /tmp.

using System.IO.Compression;
using System.Security.Cryptography;
using System.Text;
using Apiextensions.Fn.Proto.V1;
using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;

namespace DashboardBundle.Tests;

[TestClass]
public sealed class FunctionTests
{
    const string Url = "https://artifacts.example.com/dashboards/platform-v1.2.0.zip";

    string scratch = "";

    /// <summary>The scratch directory of the latest Run or Refusal: each gets
    /// a fresh one, as each run gets a fresh private /tmp.</summary>
    string lastRun = "";

    [TestInitialize]
    public void MakeScratch() => scratch = Directory.CreateTempSubdirectory("dashboard-bundle-").FullName;

    int runs;

    string Fresh() => lastRun = Directory.CreateDirectory(Path.Combine(scratch, $"run-{++runs}")).FullName;

    [TestCleanup]
    public void RemoveScratch() => Directory.Delete(scratch, true);

    static void Log(LogLevel level, string msg, params (string Key, string Value)[] kv)
    {
    }

    static string Dashboard(string uid, string title) => $$"""{"uid": "{{uid}}", "title": "{{title}}", "panels": []}""";

    /// <summary>A zip archive holding these entries; a name ending in / is a
    /// directory.</summary>
    static byte[] Zip(params (string Name, string Content)[] entries)
    {
        var ms = new MemoryStream();
        using (var zip = new ZipArchive(ms, ZipArchiveMode.Create, true))
        {
            foreach (var (name, content) in entries)
            {
                var entry = zip.CreateEntry(name);
                if (!name.EndsWith('/'))
                {
                    using var w = new StreamWriter(entry.Open());
                    w.Write(content);
                }
            }
        }
        return ms.ToArray();
    }

    static string DigestOf(byte[] b) => "sha256:" + Convert.ToHexStringLower(SHA256.HashData(b));

    static FetchBytes Serving(byte[] bundle) =>
        url => url == Url ? bundle : throw new InvalidOperationException($"GET {url}: status 404");

    static RunFunctionRequest Request(string digest, string url = Url, string folder = "Platform")
    {
        var spec = new Struct
        {
            Fields =
            {
                ["url"] = Value.ForString(url),
                ["digest"] = Value.ForString(digest),
                ["folder"] = Value.ForString(folder),
            },
        };
        return new RunFunctionRequest
        {
            Meta = new RequestMeta { Tag = "bundle" },
            Observed = new State
            {
                Composite = new Resource
                {
                    Resource_ = new Struct
                    {
                        Fields =
                        {
                            ["apiVersion"] = Value.ForString("observability.example.org/v1alpha1"),
                            ["kind"] = Value.ForString("DashboardBundle"),
                            ["metadata"] = Value.ForStruct(new Struct { Fields = { ["name"] = Value.ForString("platform") } }),
                            ["spec"] = Value.ForStruct(spec),
                        },
                    },
                },
            },
        };
    }

    RunFunctionResponse Run(byte[] bundle) => Function.RunFunction(Request(DigestOf(bundle)), Serving(bundle), Fresh(), Log);

    string Refusal(byte[] bundle) => Refusal(Request(DigestOf(bundle)), Serving(bundle));

    string Refusal(RunFunctionRequest req, FetchBytes fetch) =>
        Assert.ThrowsExactly<RunException>(() => Function.RunFunction(req, fetch, Fresh(), Log)).Message;

    static Struct ConfigMap(string name, string folder, string key, string json) => new()
    {
        Fields =
        {
            ["apiVersion"] = Value.ForString("v1"),
            ["kind"] = Value.ForString("ConfigMap"),
            ["metadata"] = Value.ForStruct(new Struct
            {
                Fields =
                {
                    ["name"] = Value.ForString(name),
                    ["labels"] = Value.ForStruct(new Struct { Fields = { ["grafana_dashboard"] = Value.ForString("1") } }),
                    ["annotations"] = Value.ForStruct(new Struct { Fields = { ["grafana_folder"] = Value.ForString(folder) } }),
                },
            }),
            ["data"] = Value.ForStruct(new Struct { Fields = { [key] = Value.ForString(json) } }),
        },
    };

    [TestMethod]
    public void ComposesOneConfigMapPerDashboard()
    {
        var overview = Dashboard("node-overview", "Node overview");
        var latency = Dashboard("api-latency", "API latency");
        var bundle = Zip(("nodes/", ""), ("nodes/overview.json", overview), ("api/latency.json", latency));
        var req = Request(DigestOf(bundle));
        req.Desired = new State { Resources = { ["earlier"] = new Resource() } };
        var logged = new List<string>();

        var rsp = Function.RunFunction(req, Serving(bundle), scratch, (level, msg, kv) => logged.Add(msg));

        Assert.AreEqual(3, rsp.Desired.Resources.Count);
        Assert.AreEqual(new Resource(), rsp.Desired.Resources["earlier"], "earlier steps' resources are kept");
        Assert.AreEqual(
            new Resource
            {
                Resource_ = ConfigMap("platform-nodes-overview", "Platform", "nodes-overview.json", overview),
                Ready = Ready.True,
            },
            rsp.Desired.Resources["nodes-overview"]);
        Assert.AreEqual(
            new Resource
            {
                Resource_ = ConfigMap("platform-api-latency", "Platform", "api-latency.json", latency),
                Ready = Ready.True,
            },
            rsp.Desired.Resources["api-latency"]);
        Assert.AreEqual(
            new Struct
            {
                Fields =
                {
                    ["count"] = Value.ForNumber(2),
                    ["digest"] = Value.ForString(DigestOf(bundle)),
                },
            },
            rsp.Desired.Composite.Resource_.Fields["status"].StructValue.Fields["dashboards"].StructValue);
        Assert.AreEqual(new ResponseMeta { Tag = "bundle", Ttl = new Duration { Seconds = 60 } }, rsp.Meta);
        Assert.AreEqual($"2 dashboards from {DigestOf(bundle)} in folder Platform", rsp.Results.Single().Message);
        Assert.AreEqual("Unpacked dashboard bundle", logged.Single());
    }

    [TestMethod]
    public void UnpacksIntoTheScratchDirectory()
    {
        Run(Zip(("a/b.json", Dashboard("b", "B"))));

        Assert.IsTrue(File.Exists(Path.Combine(lastRun, "bundle.zip")));
        Assert.IsTrue(File.Exists(Path.Combine(lastRun, "bundle", "a", "b.json")));
    }

    [TestMethod]
    [DataRow("overview.json", "overview")]
    [DataRow("nodes/Disk_Pressure.json", "nodes-disk-pressure")]
    [DataRow("API latency (p99).json", "api-latency-p99")]
    [DataRow("-team-/--slo--.json", "team-slo")]
    public void NamesFollowThePath(string path, string name) => Assert.AreEqual(name, Bundle.NameOf(path));

    [TestMethod]
    public void KeepsEarlierStatus()
    {
        var bundle = Zip(("a.json", Dashboard("a", "A")));
        var req = Request(DigestOf(bundle));
        req.Desired = new State
        {
            Composite = new Resource { Resource_ = Struct.Parser.ParseJson("""{"status": {"owner": "sre"}}""") },
        };

        var rsp = Function.RunFunction(req, Serving(bundle), scratch, Log);

        var status = rsp.Desired.Composite.Resource_.Fields["status"].StructValue;
        Assert.AreEqual("sre", status.Fields["owner"].StringValue);
        Assert.AreEqual(1, status.Fields["dashboards"].StructValue.Fields["count"].NumberValue);
    }

    [TestMethod]
    public void DigestMismatchIsRefused()
    {
        var bundle = Zip(("a.json", Dashboard("a", "A")));
        var pinned = "sha256:" + new string('0', 64);

        Assert.AreEqual(
            $"bundle {Url} has digest {DigestOf(bundle)}, but spec.digest pins {pinned}",
            Refusal(Request(pinned), Serving(bundle)));
        Assert.IsFalse(File.Exists(Path.Combine(lastRun, "bundle.zip")), "nothing is unpacked");
    }

    [TestMethod]
    [DataRow("sha256:abc")]
    [DataRow("sha512:0000000000000000000000000000000000000000000000000000000000000000")]
    [DataRow("sha256:000000000000000000000000000000000000000000000000000000000000000G")]
    public void MalformedDigestIsRefused(string digest) =>
        Assert.AreEqual(
            $"spec.digest {digest} is not sha256:<64 lowercase hex digits>",
            Refusal(Request(digest), Serving([])));

    [TestMethod]
    public void FetchFailureIsRefused() =>
        Assert.AreEqual(
            "cannot fetch bundle https://artifacts.example.com/dashboards/gone.zip: GET https://artifacts.example.com/dashboards/gone.zip: status 404",
            Refusal(Request(DigestOf([]), "https://artifacts.example.com/dashboards/gone.zip"), Serving([])));

    [TestMethod]
    public void MalformedArchiveIsRefused() =>
        StringAssert.StartsWith(
            Refusal(Encoding.UTF8.GetBytes("not a zip archive")),
            "bundle is not a valid zip archive: ");

    [TestMethod]
    [DataRow("../evil.json")]
    [DataRow("nodes/../../evil.json")]
    [DataRow("/etc/evil.json")]
    [DataRow("../notes.txt")]
    public void EntryEscapingTheExtractionDirectoryIsRefused(string name)
    {
        Assert.AreEqual(
            $"bundle entry \"{name}\" escapes the extraction directory",
            Refusal(Zip(("a.json", Dashboard("a", "A")), (name, Dashboard("e", "E")))));
        Assert.IsFalse(Directory.Exists(Path.Combine(lastRun, "bundle")), "nothing is extracted");
    }

    [TestMethod]
    public void FilesThatAreNotDashboardsAreSkippedWithAWarning()
    {
        var bundle = Zip(
            ("nodes/overview.json", Dashboard("node-overview", "Node overview")),
            ("README.md", "# Platform dashboards"),
            ("nodes/.DS_Store", "\0\0\0\u0001Bud1"),
            ("__MACOSX/", ""),
            ("__MACOSX/nodes/._overview.json", "\0\u0005\u0016\u0007"),
            ("._README.md", "\0\u0005\u0016\u0007"));
        var logged = new List<string>();

        var rsp = Function.RunFunction(
            Request(DigestOf(bundle)), Serving(bundle), Fresh(),
            (level, msg, kv) => logged.Add($"{level} {msg} {string.Join(" ", kv.Select(p => $"{p.Key}={p.Value}"))}"));

        Assert.AreEqual("nodes-overview", rsp.Desired.Resources.Keys.Single());
        Assert.AreEqual(
            1, rsp.Desired.Composite.Resource_.Fields["status"].StructValue.Fields["dashboards"].StructValue.Fields["count"].NumberValue);
        Assert.AreEqual(
            new Result
            {
                Severity = Severity.Warning,
                Message = "skipped 4 bundle files that are not dashboards: README.md, nodes/.DS_Store, "
                    + "__MACOSX/nodes/._overview.json, ._README.md",
                Target = Target.Composite,
            },
            rsp.Results[1]);
        Assert.AreEqual(2, rsp.Results.Count);
        CollectionAssert.AreEqual(
            new[]
            {
                $"Warn Skipped a bundle file that is not a dashboard bundle={Url} file=README.md",
                $"Warn Skipped a bundle file that is not a dashboard bundle={Url} file=nodes/.DS_Store",
                $"Warn Skipped a bundle file that is not a dashboard bundle={Url} file=__MACOSX/nodes/._overview.json",
                $"Warn Skipped a bundle file that is not a dashboard bundle={Url} file=._README.md",
            },
            logged.Take(4).ToArray());
        var extracted = Directory.EnumerateFileSystemEntries(Path.Combine(lastRun, "bundle"), "*", SearchOption.AllDirectories)
            .Select(f => Path.GetRelativePath(Path.Combine(lastRun, "bundle"), f))
            .Order(StringComparer.Ordinal);
        CollectionAssert.AreEqual(new[] { "nodes", "nodes/overview.json" }, extracted.ToArray(), "skipped files are never written");
    }

    [TestMethod]
    public void OneSkippedFileAndOneDashboardReadInTheSingular()
    {
        var bundle = Zip(("a.json", Dashboard("a", "A")), ("README.md", "# A"));
        var rsp = Run(bundle);

        Assert.AreEqual($"1 dashboard from {DigestOf(bundle)} in folder Platform", rsp.Results[0].Message);
        Assert.AreEqual("skipped 1 bundle file that is not a dashboard: README.md", rsp.Results[1].Message);
    }

    [TestMethod]
    public void TheWarningNamesTenSkippedFilesAndCountsTheRest()
    {
        var junk = Enumerable.Range(0, 15).Select(i => ($"notes/{i:00}.txt", "junk"));
        var rsp = Run(Zip([("a.json", Dashboard("a", "A")), .. junk]));

        Assert.AreEqual(
            "skipped 15 bundle files that are not dashboards: notes/00.txt, notes/01.txt, notes/02.txt, notes/03.txt, "
                + "notes/04.txt, notes/05.txt, notes/06.txt, notes/07.txt, notes/08.txt, notes/09.txt and 5 more",
            rsp.Results[1].Message);
    }

    [TestMethod]
    public void JsonFileThatIsNotADashboardIsRefused()
    {
        StringAssert.StartsWith(
            Refusal(Zip(("broken.json", "{\"uid\": "))),
            "bundle file broken.json is not JSON: ");
        Assert.AreEqual(
            "bundle file list.json is not a dashboard: not a JSON object",
            Refusal(Zip(("list.json", "[]"))));
        Assert.AreEqual(
            "bundle file untitled.json is not a dashboard: it needs a uid and a title",
            Refusal(Zip(("untitled.json", """{"uid": "u"}"""))));
        Assert.AreEqual(
            "bundle file anonymous.json is not a dashboard: it needs a uid and a title",
            Refusal(Zip(("anonymous.json", """{"uid": "", "title": "T"}"""))));
    }

    [TestMethod]
    public void DashboardOverAConfigMapIsRefused()
    {
        var big = $$"""{"uid": "big", "title": "Big", "description": "{{new string('x', 1 << 20)}}"}""";
        var size = Encoding.UTF8.GetByteCount(big);

        Assert.AreEqual(
            $"bundle file big.json is {size} bytes, more than the 1048576 a ConfigMap holds",
            Refusal(Zip(("big.json", big))));
    }

    [TestMethod]
    public void TooManyDashboardsAreRefused()
    {
        var dashboards = Enumerable.Range(0, Bundle.MaxDashboards + 1)
            .Select(i => ($"d{i}.json", Dashboard($"d{i}", $"D{i}")))
            .ToArray();

        Assert.AreEqual("bundle holds more than 64 dashboards", Refusal(Zip(dashboards)));
        Assert.AreEqual(
            Bundle.MaxDashboards,
            Run(Zip([.. dashboards[..Bundle.MaxDashboards], ("README.md", "skipped, not counted")])).Desired.Resources.Count);
    }

    [TestMethod]
    public void TooManyEntriesAreRefused()
    {
        var junk = Enumerable.Range(0, Bundle.MaxEntries).Select(i => ($"junk/{i}.txt", ""));

        Assert.AreEqual(
            "bundle holds more than 1024 entries",
            Refusal(Zip([("a.json", Dashboard("a", "A")), .. junk])));
        Assert.AreEqual(1, Run(Zip([("a.json", Dashboard("a", "A")), .. junk.Skip(1)])).Desired.Resources.Count);
    }

    [TestMethod]
    public void BundleWithoutDashboardsIsRefused()
    {
        Assert.AreEqual("bundle holds no dashboards (.json files)", Refusal(Zip(("nodes/", ""))));
        Assert.AreEqual(
            "bundle holds no dashboards (.json files)",
            Refusal(Zip(("README.md", "# nothing yet"), ("__MACOSX/._README.md", ""))));
    }

    [TestMethod]
    public void NameCollisionIsRefused() =>
        Assert.AreEqual(
            "bundle files node-overview.json and node_overview.json would both be named node-overview",
            Refusal(Zip(("node-overview.json", Dashboard("a", "A")), ("node_overview.json", Dashboard("b", "B")))));

    [TestMethod]
    public void UnnameableFileIsRefused() =>
        Assert.AreEqual(
            "bundle file ___.json has no letters or digits to name its ConfigMap after",
            Refusal(Zip(("___.json", Dashboard("a", "A")))));

    [TestMethod]
    public void SharedUidIsRefused() =>
        Assert.AreEqual(
            "bundle files a.json and b.json share the dashboard uid same",
            Refusal(Zip(("a.json", Dashboard("same", "A")), ("b.json", Dashboard("same", "B")))));

    [TestMethod]
    public void MissingScratchIsRefused()
    {
        var bundle = Zip(("a.json", Dashboard("a", "A")));
        var missing = Path.Combine(scratch, "missing");

        var e = Assert.ThrowsExactly<RunException>(
            () => Function.RunFunction(Request(DigestOf(bundle)), Serving(bundle), missing, Log));
        StringAssert.StartsWith(e.Message, $"cannot unpack the bundle under {missing}: ");
    }

    [TestMethod]
    public void IncompleteSpecIsRefused()
    {
        var req = Request(DigestOf([]));
        req.Observed.Composite.Resource_.Fields["spec"].StructValue.Fields.Remove("folder");
        Assert.AreEqual("spec.folder is required", Refusal(req, Serving([])));

        req.Observed.Composite.Resource_.Fields["spec"].StructValue.Fields["folder"] = Value.ForNumber(1);
        Assert.AreEqual("spec.folder must be a string", Refusal(req, Serving([])));

        Assert.AreEqual(
            "cannot get observed composite resource: none in request",
            Refusal(new RunFunctionRequest(), Serving([])));
    }

    [TestMethod]
    public void HandleReportsRefusalsAsFatalResults()
    {
        var req = Request("sha256:" + new string('0', 64));
        var rsp = RunFunctionResponse.Parser.ParseFrom(
            Function.Handle(req.ToByteArray(), Serving(Zip(("a.json", Dashboard("a", "A")))), scratch, Log));

        Assert.AreEqual(new ResponseMeta { Tag = "bundle", Ttl = new Duration { Seconds = 60 } }, rsp.Meta);
        Assert.AreEqual(Severity.Fatal, rsp.Results.Single().Severity);
        StringAssert.StartsWith(rsp.Results.Single().Message, $"bundle {Url} has digest sha256:");
        Assert.IsNull(rsp.Desired);
    }

    [TestMethod]
    public void HandleRunsAndEncodes()
    {
        var bundle = Zip(("a.json", Dashboard("a", "A")));
        var req = Request(DigestOf(bundle));
        var rsp = RunFunctionResponse.Parser.ParseFrom(Function.Handle(req.ToByteArray(), Serving(bundle), scratch, Log));

        Assert.AreEqual(Severity.Normal, rsp.Results.Single().Severity);
        Assert.IsTrue(rsp.Desired.Resources.ContainsKey("a"));
    }

    [TestMethod]
    public void HandleUndecodableRequestIsFatal()
    {
        var rsp = RunFunctionResponse.Parser.ParseFrom(Function.Handle([0xff], Serving([]), scratch, Log));

        StringAssert.StartsWith(rsp.Results.Single().Message, "cannot decode RunFunctionRequest: ");
        Assert.AreEqual(Severity.Fatal, rsp.Results.Single().Severity);
        Assert.IsNull(rsp.Meta);
    }
}

[TestClass]
public sealed class Sha256Tests
{
    [TestMethod]
    [DataRow("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")]
    [DataRow("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")]
    [DataRow(
        "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")]
    public void MatchesTheFipsVectors(string message, string hex) =>
        Assert.AreEqual(hex, Sha256.Hex(Encoding.ASCII.GetBytes(message)));

    [TestMethod]
    public void MatchesDotNetAcrossBlockBoundaries()
    {
        var random = new Random(180);
        for (var length = 0; length <= 300; length++)
        {
            var data = new byte[length];
            random.NextBytes(data);
            Assert.AreEqual(Convert.ToHexStringLower(SHA256.HashData(data)), Sha256.Hex(data), $"length {length}");
        }
    }
}
