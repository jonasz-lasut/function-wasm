// Unpacking a dashboard bundle: the zip archive is written into the run's
// scratch directory (the private /tmp on the wasm target), every entry is
// checked, each dashboard is extracted into a directory tree with
// ZipArchiveEntry.ExtractToFile, and the tree is walked. The checks run
// before anything is extracted, so a bundle that breaks a rule writes
// nothing but the archive itself, and a file that is not a dashboard (a
// README, macOS metadata) is skipped without ever being written.

using System.IO.Compression;
using System.Text;
using System.Text.Json;

namespace DashboardBundle;

/// <summary>One dashboard of a bundle: where it sits in the bundle, the
/// name it is composed under, and its JSON as the bundle carries it.</summary>
public sealed record Dashboard(string Path, string Name, string Json);

/// <summary>A bundle's dashboards, in path order, and the files it skipped
/// because they are not dashboards, in entry order.</summary>
public sealed record Unpacked(List<Dashboard> Dashboards, List<string> Skipped);

public static class Bundle
{
    /// <summary>The most dashboards a bundle may hold.</summary>
    public const int MaxDashboards = 64;

    /// <summary>The most entries a bundle may hold, skipped files and
    /// directories included, so a bundle full of anything else stays
    /// bounded.</summary>
    public const int MaxEntries = 1024;

    /// <summary>The largest dashboard: what one ConfigMap holds.</summary>
    public const long MaxDashboardBytes = 1 << 20;

    /// <summary>Unpacks <paramref name="archive"/> under <paramref name="scratch"/>.</summary>
    public static Unpacked Unpack(byte[] archive, string scratch)
    {
        var zipPath = Path.Combine(scratch, "bundle.zip");
        var root = Path.Combine(scratch, "bundle");
        try
        {
            File.WriteAllBytes(zipPath, archive);
            List<string> skipped;
            try
            {
                using var zip = ZipFile.OpenRead(zipPath);
                (var dashboards, skipped) = Check(zip, root);
                foreach (var entry in dashboards)
                {
                    var target = Path.Combine(root, entry.FullName);
                    Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                    entry.ExtractToFile(target);
                }
            }
            catch (InvalidDataException e)
            {
                throw new RunException($"bundle is not a valid zip archive: {e.Message}");
            }
            return new Unpacked(Walk(root), skipped);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            throw new RunException($"cannot unpack the bundle under {scratch}: {e.Message}");
        }
    }

    /// <summary>Sorts a bundle's entries before anything is extracted into
    /// the dashboards to extract and the files to skip, and refuses the
    /// bundle for an entry that would land outside the extraction directory
    /// (zip slip, skipped files included), a dashboard larger than a
    /// ConfigMap holds, too many entries or dashboards, or no dashboard.</summary>
    static (List<ZipArchiveEntry> Dashboards, List<string> Skipped) Check(ZipArchive zip, string root)
    {
        var inside = Path.GetFullPath(root) + Path.DirectorySeparatorChar;
        var dashboards = new List<ZipArchiveEntry>();
        var skipped = new List<string>();
        var entries = 0;
        foreach (var entry in zip.Entries)
        {
            if (++entries > MaxEntries)
            {
                throw new RunException($"bundle holds more than {MaxEntries} entries");
            }
            var target = Path.GetFullPath(Path.Combine(root, entry.FullName));
            if (!target.StartsWith(inside, StringComparison.Ordinal))
            {
                throw new RunException($"bundle entry \"{entry.FullName}\" escapes the extraction directory");
            }
            if (entry.FullName.EndsWith('/'))
            {
                continue;
            }
            if (!IsDashboard(entry.FullName))
            {
                skipped.Add(entry.FullName);
                continue;
            }
            if (dashboards.Count == MaxDashboards)
            {
                throw new RunException($"bundle holds more than {MaxDashboards} dashboards");
            }
            if (entry.Length > MaxDashboardBytes)
            {
                throw new RunException(
                    $"bundle file {entry.FullName} is {entry.Length} bytes, more than the {MaxDashboardBytes} a ConfigMap holds");
            }
            dashboards.Add(entry);
        }
        if (dashboards.Count == 0)
        {
            throw new RunException("bundle holds no dashboards (.json files)");
        }
        return (dashboards, skipped);
    }

    /// <summary>Whether a bundle file is meant as a dashboard: a .json file,
    /// unless it is macOS metadata - an AppleDouble ._ file, or anything
    /// under the __MACOSX directory a Finder-made zip carries, where every
    /// dashboard has a ._ twin ending in .json too.</summary>
    static bool IsDashboard(string path)
    {
        var segments = path.Split('/');
        return path.EndsWith(".json", StringComparison.Ordinal)
            && !segments[^1].StartsWith("._", StringComparison.Ordinal)
            && !segments.Contains("__MACOSX");
    }

    /// <summary>Reads every extracted dashboard, in path order.</summary>
    static List<Dashboard> Walk(string root)
    {
        var dashboards = new List<Dashboard>();
        var byName = new Dictionary<string, string>();
        var byUid = new Dictionary<string, string>();
        var paths = Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories)
            .Select(f => Path.GetRelativePath(root, f).Replace('\\', '/'))
            .Order(StringComparer.Ordinal);
        foreach (var path in paths)
        {
            var json = File.ReadAllText(Path.Combine(root, path), Encoding.UTF8);
            var uid = Validate(path, json);
            var name = NameOf(path);
            if (byName.TryGetValue(name, out var other))
            {
                throw new RunException($"bundle files {other} and {path} would both be named {name}");
            }
            if (byUid.TryGetValue(uid, out other))
            {
                throw new RunException($"bundle files {other} and {path} share the dashboard uid {uid}");
            }
            byName[name] = path;
            byUid[uid] = path;
            dashboards.Add(new Dashboard(path, name, json));
        }
        return dashboards;
    }

    /// <summary>Checks a file is a Grafana dashboard - a JSON object with a
    /// uid and a title - and returns its uid.</summary>
    static string Validate(string path, string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                throw new RunException($"bundle file {path} is not a dashboard: not a JSON object");
            }
            var uid = NonEmptyString(root, "uid");
            if (uid is null || NonEmptyString(root, "title") is null)
            {
                throw new RunException($"bundle file {path} is not a dashboard: it needs a uid and a title");
            }
            return uid;
        }
        catch (JsonException e)
        {
            throw new RunException($"bundle file {path} is not JSON: {e.Message}");
        }
    }

    static string? NonEmptyString(JsonElement o, string key) =>
        o.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String && v.GetString() is { Length: > 0 } s
            ? s
            : null;

    /// <summary>The name a dashboard is composed under, from its path in the
    /// bundle: lowercase letters and digits, every other run of characters a
    /// single hyphen, ".json" dropped - nodes/Disk_Pressure.json is
    /// nodes-disk-pressure.</summary>
    public static string NameOf(string path)
    {
        var stem = path[..^".json".Length];
        var name = new StringBuilder(stem.Length);
        foreach (var c in stem.ToLowerInvariant())
        {
            if (char.IsAsciiLetterOrDigit(c))
            {
                name.Append(c);
            }
            else if (name.Length > 0 && name[^1] != '-')
            {
                name.Append('-');
            }
        }
        var trimmed = name.ToString().TrimEnd('-');
        if (trimmed.Length == 0)
        {
            throw new RunException($"bundle file {path} has no letters or digits to name its ConfigMap after");
        }
        return trimmed;
    }
}
