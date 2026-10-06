// The dashboard-bundle function: fetches the bundle a DashboardBundle
// composite resource pins (a zip archive of Grafana dashboards), checks its
// sha256 against the pin, unpacks it under the run's scratch directory - the
// private /tmp the runtime pre-opens on the wasm target - and composes one
// ConfigMap per dashboard for Grafana's dashboard sidecar. It works over the
// protobuf messages protoc generated from the vendored crossplane proto
// (src/Gen, Google.Protobuf); the fetch is wasi:http through the host on the
// wasm target and a test double natively, so this file builds and tests
// under plain .NET.

using Apiextensions.Fn.Proto.V1;
using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;

namespace DashboardBundle;

/// <summary>GETs a URL and returns its body, or throws.</summary>
public delegate byte[] FetchBytes(string url);

/// <summary>One structured log line through the host.</summary>
public delegate void Log(LogLevel level, string msg, params (string Key, string Value)[] kv);

public enum LogLevel
{
    Debug,
    Info,
    Warn,
    Error,
}

/// <summary>A failure whose message is the fatal result.</summary>
public sealed class RunException(string message) : Exception(message);

public static class Function
{
    const long DefaultTtlSeconds = 60;

    /// <summary>Where the runtime pre-opens the run's private /tmp.</summary>
    public const string PrivateTmp = "/tmp";

    /// <summary>The label Grafana's dashboard sidecar selects ConfigMaps by.</summary>
    public const string SidecarLabel = "grafana_dashboard";

    /// <summary>The annotation the sidecar reads the dashboard's folder from
    /// (the Grafana chart's sidecar.dashboards.folderAnnotation).</summary>
    public const string FolderAnnotation = "grafana_folder";

    /// <summary>Composes one ConfigMap per dashboard of the pinned bundle,
    /// keeping everything earlier steps composed.</summary>
    public static RunFunctionResponse RunFunction(
        RunFunctionRequest req, FetchBytes fetch, string scratch, Log log)
    {
        var tag = req.Meta?.Tag ?? "";
        var xr = req.Observed?.Composite?.Resource_
            ?? throw new RunException("cannot get observed composite resource: none in request");
        var name = StringField(StructField(xr, "metadata"), "name", "metadata") ?? "";
        var spec = StructField(xr, "spec");
        var url = RequiredString(spec, "url");
        var digest = RequiredString(spec, "digest");
        var folder = RequiredString(spec, "folder");
        if (!IsSha256Digest(digest))
        {
            throw new RunException($"spec.digest {digest} is not sha256:<64 lowercase hex digits>");
        }

        byte[] archive;
        try
        {
            archive = fetch(url);
        }
        catch (Exception e)
        {
            throw new RunException($"cannot fetch bundle {url}: {e.Message}");
        }
        // The pin, not the URL, decides what is applied: a tag moved to other
        // content is refused, and Crossplane keeps what it composed last time.
        var actual = "sha256:" + Sha256.Hex(archive);
        if (actual != digest)
        {
            throw new RunException($"bundle {url} has digest {actual}, but spec.digest pins {digest}");
        }

        var (dashboards, skipped) = Bundle.Unpack(archive, scratch);
        foreach (var file in skipped)
        {
            // The log import has only debug and info; the warning result
            // below is what reaches the team, as an event on the XR.
            log(LogLevel.Info, "Skipped a bundle file that is not a dashboard", ("bundle", url), ("file", file));
        }
        log(LogLevel.Info, "Unpacked dashboard bundle",
            ("bundle", url), ("digest", digest), ("dashboards", dashboards.Count.ToString()));

        var desired = req.Desired?.Clone() ?? new State();
        foreach (var d in dashboards)
        {
            // A ConfigMap has no status to wait for: it is ready once it
            // exists, which is what Crossplane's own Ready condition counts.
            desired.Resources[d.Name] = new Resource
            {
                Resource_ = ConfigMap($"{name}-{d.Name}", folder, d),
                Ready = Ready.True,
            };
        }
        SetStatus(desired, dashboards.Count, digest);

        var rsp = new RunFunctionResponse
        {
            Meta = new ResponseMeta { Tag = tag, Ttl = new Duration { Seconds = DefaultTtlSeconds } },
            Desired = desired,
            Results =
            {
                new Result
                {
                    Severity = Severity.Normal,
                    Message = $"{Count(dashboards.Count, "dashboard")} from {digest} in folder {folder}",
                    Target = Target.Composite,
                },
            },
        };
        if (skipped.Count > 0)
        {
            rsp.Results.Add(new Result
            {
                Severity = Severity.Warning,
                Message = SkippedMessage(skipped),
                Target = Target.Composite,
            });
        }
        return rsp;
    }

    /// <summary>The most skipped files a warning names; the rest are counted.</summary>
    const int NamedSkippedFiles = 10;

    /// <summary>One warning for all the skipped files, naming the first few,
    /// so the event stays readable however much else a bundle carries.</summary>
    static string SkippedMessage(List<string> skipped)
    {
        var named = string.Join(", ", skipped.Take(NamedSkippedFiles));
        var more = skipped.Count > NamedSkippedFiles ? $" and {skipped.Count - NamedSkippedFiles} more" : "";
        var what = skipped.Count == 1 ? "is not a dashboard" : "are not dashboards";
        return $"skipped {Count(skipped.Count, "bundle file")} that {what}: {named}{more}";
    }

    static string Count(int n, string noun) => n == 1 ? $"1 {noun}" : $"{n} {noun}s";

    /// <summary>Decode, run in the private /tmp, encode. Every failure
    /// becomes a fatal result so the host can always decode the reply.</summary>
    public static byte[] Handle(byte[] input, FetchBytes fetch, Log log) => Handle(input, fetch, PrivateTmp, log);

    public static byte[] Handle(byte[] input, FetchBytes fetch, string scratch, Log log)
    {
        RunFunctionRequest req;
        try
        {
            req = RunFunctionRequest.Parser.ParseFrom(input);
        }
        catch (InvalidProtocolBufferException e)
        {
            return Fatal(null, $"cannot decode RunFunctionRequest: {e.Message}").ToByteArray();
        }
        try
        {
            return RunFunction(req, fetch, scratch, log).ToByteArray();
        }
        catch (RunException e)
        {
            return Fatal(req, e.Message).ToByteArray();
        }
    }

    /// <summary>A dashboard as the sidecar expects it: labelled, its folder
    /// annotated, the JSON under one data key.</summary>
    static Struct ConfigMap(string name, string folder, Dashboard d) => new()
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
                    ["labels"] = Value.ForStruct(new Struct
                    {
                        Fields = { [SidecarLabel] = Value.ForString("1") },
                    }),
                    ["annotations"] = Value.ForStruct(new Struct
                    {
                        Fields = { [FolderAnnotation] = Value.ForString(folder) },
                    }),
                },
            }),
            ["data"] = Value.ForStruct(new Struct
            {
                Fields = { [$"{d.Name}.json"] = Value.ForString(d.Json) },
            }),
        },
    };

    /// <summary>Sets status.dashboards on the desired composite resource,
    /// keeping whatever earlier steps put there.</summary>
    static void SetStatus(State desired, int count, string digest)
    {
        desired.Composite ??= new Resource();
        desired.Composite.Resource_ ??= new Struct();
        var xr = desired.Composite.Resource_;
        if (StructField(xr, "status") is null)
        {
            xr.Fields["status"] = Value.ForStruct(new Struct());
        }
        xr.Fields["status"].StructValue.Fields["dashboards"] = Value.ForStruct(new Struct
        {
            Fields =
            {
                ["count"] = Value.ForNumber(count),
                ["digest"] = Value.ForString(digest),
            },
        });
    }

    static RunFunctionResponse Fatal(RunFunctionRequest? req, string message)
    {
        var rsp = new RunFunctionResponse
        {
            Results = { new Result { Severity = Severity.Fatal, Message = message, Target = Target.Composite } },
        };
        if (req is not null)
        {
            rsp.Meta = new ResponseMeta
            {
                Tag = req.Meta?.Tag ?? "",
                Ttl = new Duration { Seconds = DefaultTtlSeconds },
            };
        }
        return rsp;
    }

    static bool IsSha256Digest(string s) =>
        s.Length == "sha256:".Length + 64
        && s.StartsWith("sha256:", StringComparison.Ordinal)
        && s["sha256:".Length..].All(c => char.IsAsciiDigit(c) || c is >= 'a' and <= 'f');

    static string RequiredString(Struct? spec, string key) =>
        StringField(spec, key, "spec") is { Length: > 0 } s ? s : throw new RunException($"spec.{key} is required");

    static Struct? StructField(Struct? s, string key) =>
        s is not null && s.Fields.TryGetValue(key, out var v) && v.KindCase == Value.KindOneofCase.StructValue
            ? v.StructValue
            : null;

    static string? StringField(Struct? s, string key, string context)
    {
        if (s is null || !s.Fields.TryGetValue(key, out var v) || v.KindCase == Value.KindOneofCase.NullValue)
        {
            return null;
        }
        if (v.KindCase != Value.KindOneofCase.StringValue)
        {
            throw new RunException($"{context}.{key} must be a string");
        }
        return v.StringValue;
    }
}
