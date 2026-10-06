// The hello-dotnet guest's logic: the same greeting function as every
// example, over the protobuf messages protoc generated from the vendored
// crossplane proto (src/Gen, Google.Protobuf). FetchText resolves
// config.greetingUrl - wasi:http through the host on the wasm target, a test
// double natively - so this file builds and tests under plain .NET.

using Apiextensions.Fn.Proto.V1;
using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;

namespace HelloDotnet;

/// <summary>GETs a URL and returns its trimmed body, or throws.</summary>
public delegate string FetchText(string url);

/// <summary>One structured log line through the host.</summary>
public delegate void Log(LogLevel level, string msg, params (string Key, string Value)[] kv);

public enum LogLevel
{
    Debug,
    Info,
}

/// <summary>A failure whose message is the fatal result, worded like the other guests'.</summary>
public sealed class RunException(string message) : Exception(message);

public static class Function
{
    const long DefaultTtlSeconds = 60;

    /// <summary>Adds a ConfigMap greeting the composite resource to the desired state.</summary>
    public static RunFunctionResponse RunFunction(
        RunFunctionRequest req, FetchText fetchText, Log log)
    {
        var tag = req.Meta?.Tag ?? "";
        log(LogLevel.Info, "Running function", ("tag", tag));

        var config = StructField(req.Input, "config");
        var greeting = StringField(config, "greeting", "cannot read config") ?? "hello";
        // greetingUrl fetches the greeting through the host instead - the
        // requires.egress grant of the module's manifest decides whether it may.
        var url = StringField(config, "greetingUrl", "cannot read config");
        if (url is not null)
        {
            try
            {
                greeting = fetchText(url);
            }
            catch (Exception e)
            {
                throw new RunException($"cannot fetch greeting: {e.Message}");
            }
        }

        var composite = req.Observed?.Composite?.Resource_
            ?? throw new RunException("cannot get observed composite resource: none in request");
        var name = StringField(StructField(composite, "metadata"), "name", "cannot read metadata") ?? "";

        var desired = req.Desired ?? new State();
        desired.Resources["greeting"] = new Resource
        {
            Resource_ = new Struct
            {
                Fields =
                {
                    ["apiVersion"] = Value.ForString("v1"),
                    ["kind"] = Value.ForString("ConfigMap"),
                    ["data"] = Value.ForStruct(new Struct
                    {
                        Fields = { ["greeting"] = Value.ForString($"{greeting} {name}") },
                    }),
                },
            },
        };

        return new RunFunctionResponse
        {
            Meta = new ResponseMeta { Tag = tag, Ttl = new Duration { Seconds = DefaultTtlSeconds } },
            Desired = desired,
            Results =
            {
                new Result { Severity = Severity.Normal, Message = $"greeted {name}", Target = Target.Composite },
            },
            Conditions =
            {
                new Condition
                {
                    Type = "FunctionSuccess",
                    Status = Status.ConditionTrue,
                    Reason = "Success",
                    Target = Target.CompositeAndClaim,
                },
            },
        };
    }

    /// <summary>Decode, run, encode. Every failure becomes a fatal result so
    /// the host can always decode the reply.</summary>
    public static byte[] Handle(byte[] input, FetchText fetchText, Log log)
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
            return RunFunction(req, fetchText, log).ToByteArray();
        }
        catch (RunException e)
        {
            return Fatal(req, e.Message).ToByteArray();
        }
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

    /// <summary>Reads a Struct field's sub-object.</summary>
    static Struct? StructField(Struct? s, string key) =>
        s is not null && s.Fields.TryGetValue(key, out var v) && v.KindCase == Value.KindOneofCase.StructValue
            ? v.StructValue
            : null;

    /// <summary>Reads a string field of a Struct, refusing non-strings the way
    /// the other guests word it.</summary>
    static string? StringField(Struct? s, string key, string context)
    {
        if (s is null || !s.Fields.TryGetValue(key, out var v) || v.KindCase == Value.KindOneofCase.NullValue)
        {
            return null;
        }
        if (v.KindCase != Value.KindOneofCase.StringValue)
        {
            throw new RunException($"{context}: {key} must be a string");
        }
        return v.StringValue;
    }
}
