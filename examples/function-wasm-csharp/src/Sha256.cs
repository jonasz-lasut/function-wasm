// SHA-256 (FIPS 180-4) in managed code: System.Security.Cryptography is
// built as PlatformNotSupported for WASI, so the bundle's pin is checked with
// this. The native tests hold it against .NET's own SHA256 and the FIPS
// test vectors.

using System.Buffers.Binary;

namespace DashboardBundle;

public static class Sha256
{
    static readonly uint[] K =
    [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    /// <summary>The digest of <paramref name="data"/> as lowercase hex.</summary>
    public static string Hex(ReadOnlySpan<byte> data) => Convert.ToHexStringLower(Hash(data));

    public static byte[] Hash(ReadOnlySpan<byte> data)
    {
        uint[] h = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        var w = new uint[64];

        var whole = data.Length / 64 * 64;
        for (var i = 0; i < whole; i += 64)
        {
            Compress(h, w, data.Slice(i, 64));
        }

        // The tail, the 0x80 marker and the bit length fill one or two final
        // blocks.
        var rest = data.Length - whole;
        Span<byte> tail = stackalloc byte[rest < 56 ? 64 : 128];
        tail.Clear();
        data[whole..].CopyTo(tail);
        tail[rest] = 0x80;
        BinaryPrimitives.WriteUInt64BigEndian(tail[^8..], (ulong)data.Length * 8);
        for (var i = 0; i < tail.Length; i += 64)
        {
            Compress(h, w, tail.Slice(i, 64));
        }

        var digest = new byte[32];
        for (var i = 0; i < 8; i++)
        {
            BinaryPrimitives.WriteUInt32BigEndian(digest.AsSpan(i * 4), h[i]);
        }
        return digest;
    }

    static void Compress(uint[] h, uint[] w, ReadOnlySpan<byte> block)
    {
        for (var t = 0; t < 16; t++)
        {
            w[t] = BinaryPrimitives.ReadUInt32BigEndian(block[(t * 4)..]);
        }
        for (var t = 16; t < 64; t++)
        {
            var s0 = uint.RotateRight(w[t - 15], 7) ^ uint.RotateRight(w[t - 15], 18) ^ (w[t - 15] >> 3);
            var s1 = uint.RotateRight(w[t - 2], 17) ^ uint.RotateRight(w[t - 2], 19) ^ (w[t - 2] >> 10);
            w[t] = w[t - 16] + s0 + w[t - 7] + s1;
        }

        uint a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
        for (var t = 0; t < 64; t++)
        {
            var t1 = hh + (uint.RotateRight(e, 6) ^ uint.RotateRight(e, 11) ^ uint.RotateRight(e, 25))
                + ((e & f) ^ (~e & g)) + K[t] + w[t];
            var t2 = (uint.RotateRight(a, 2) ^ uint.RotateRight(a, 13) ^ uint.RotateRight(a, 22))
                + ((a & b) ^ (a & c) ^ (b & c));
            hh = g;
            g = f;
            f = e;
            e = d + t1;
            d = c;
            c = b;
            b = a;
            a = t1 + t2;
        }
        h[0] += a;
        h[1] += b;
        h[2] += c;
        h[3] += d;
        h[4] += e;
        h[5] += f;
        h[6] += g;
        h[7] += hh;
    }
}
