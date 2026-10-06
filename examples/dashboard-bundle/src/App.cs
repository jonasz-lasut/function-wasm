// The world wiring, wasm-only: componentize-dotnet runs wit-bindgen over
// wit/ at build time (the FunctionWorld namespace: the `run` export's
// interface, the typed `log` import and the wasi:http@0.2 types), and this
// file implements the world's `run` export over those bindings. `run` is
// declared sync in this guest's wit (wit-bindgen's C# async bindings do not
// compile for this world yet; a sync-lifted function satisfies the
// runtime's async world): the bundle fetch blocks on wasi:io pollables, and
// wasi:http@0.2's outgoing-handler rides the host's egress policy. The run
// unpacks into the private /tmp the runtime pre-opens (Function.PrivateTmp).

using FunctionWorld.wit.Imports.wasi.http.v0_2_12;
using FunctionWorld.wit.Imports.wasi.io.v0_2_12;
using DashboardBundle;

namespace FunctionWorld;

public class FunctionWorldExportsImpl : IFunctionWorldExports
{
    public static byte[] Run(byte[] request) => Function.Handle(request, Http.FetchBytes, Log);

    static void Log(DashboardBundle.LogLevel level, string msg, params (string Key, string Value)[] kv) =>
        IFunctionWorldImports.Log(
            level == DashboardBundle.LogLevel.Debug ? LogLevel.DEBUG : LogLevel.INFO, msg, [.. kv]);
}

static class Http
{
    /// <summary>GETs a URL through the host and returns the body.</summary>
    public static byte[] FetchBytes(string url)
    {
        ITypesImports.Scheme scheme;
        string rest;
        if (url.StartsWith("https://", StringComparison.Ordinal))
        {
            (scheme, rest) = (ITypesImports.Scheme.Https(), url["https://".Length..]);
        }
        else if (url.StartsWith("http://", StringComparison.Ordinal))
        {
            (scheme, rest) = (ITypesImports.Scheme.Http(), url["http://".Length..]);
        }
        else
        {
            throw new InvalidOperationException($"GET {url}: only http and https URLs work");
        }
        var slash = rest.IndexOf('/');
        var (authority, path) = slash < 0 ? (rest, "/") : (rest[..slash], rest[slash..]);

        using var request = new ITypesImports.OutgoingRequest(new ITypesImports.Fields());
        request.SetMethod(ITypesImports.Method.Get());
        request.SetScheme(scheme);
        request.SetAuthority(authority);
        request.SetPathWithQuery(path);

        ITypesImports.FutureIncomingResponse pending;
        try
        {
            pending = IOutgoingHandlerImports.Handle(request, null);
        }
        catch (WitException<ITypesImports.ErrorCode> e)
        {
            throw Failed(url, e.TypedValue);
        }
        using (pending)
        {
            // Resources are dropped in reverse: the body stream before the
            // body, the body before the response.
            using var response = Await(url, pending);
            var status = response.Status();
            using var body = response.Consume();
            var bytes = ReadToEnd(body);
            if (status != 200)
            {
                throw new InvalidOperationException($"GET {url}: status {status}");
            }
            return bytes;
        }
    }

    static ITypesImports.IncomingResponse Await(string url, ITypesImports.FutureIncomingResponse pending)
    {
        using (var ready = pending.Subscribe())
        {
            ready.Block();
        }
        // some(ok(...)) once ready; some(err) only for a second get.
        var outcome = pending.Get() ?? throw new InvalidOperationException($"GET {url}: no response");
        if (outcome.IsErr)
        {
            throw new InvalidOperationException($"GET {url}: the response was already taken");
        }
        var response = outcome.AsOk;
        return response.IsOk ? response.AsOk : throw Failed(url, response.AsErr);
    }

    static byte[] ReadToEnd(ITypesImports.IncomingBody body)
    {
        using var stream = body.Stream();
        var buf = new MemoryStream();
        while (true)
        {
            try
            {
                buf.Write(stream.BlockingRead(64 * 1024));
            }
            catch (WitException<IStreamsImports.StreamError> e)
                when (e.TypedValue.Tag == IStreamsImports.StreamError.Tags.Closed)
            {
                return buf.ToArray();
            }
        }
    }

    /// <summary>The error-code as the exception the fetch reports: the
    /// runtime puts its own refusal wording in internal-error.</summary>
    static InvalidOperationException Failed(string url, ITypesImports.ErrorCode code) =>
        new(code.Tag == ITypesImports.ErrorCode.Tags.InternalError
            ? $"GET {url}: internal-error: {code.AsInternalError}"
            : $"GET {url}: error-code {code.Tag}");
}
