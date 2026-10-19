// MarsMaker PNG decoder: pure C# (no UnityEngine), so it is unit-testable outside Unity.
//
// Why: Texture2D.LoadImage / ImageConversion reduce 16-bit PNGs to 8 bits, which destroys the
// MarsMaker height encoding (uint16, 0 = nodata). This decoder keeps full 16-bit precision.
//
// Supports non-interlaced PNGs, bit depth 8 or 16, colour types 0 (grey), 2 (RGB), 4 (grey+alpha),
// 6 (RGBA). Palette, sub-byte depths and Adam7 interlacing are rejected with an exception.
// Uses System.IO.Compression.DeflateStream (available in Unity's .NET Standard 2.1 and .NET 4.x
// profiles); the 2-byte zlib header is skipped by hand because ZLibStream needs .NET 6+.
using System;
using System.IO;
using System.IO.Compression;

namespace MarsMaker.Core
{
    public sealed class PngImage
    {
        public int Width;
        public int Height;
        public int BitDepth;     // 8 or 16
        public int ColorType;    // 0, 2, 4, 6
        public int Channels;     // 1..4
        public int RowBytes;     // bytes per unfiltered row (no filter byte)
        public byte[] Pixels;    // unfiltered rows, row 0 = top of image, big-endian samples for 16-bit

        /// <summary>Sample of channel ch at (x, y), y = 0 is the top row. 0..255 or 0..65535.</summary>
        public int Sample(int x, int y, int ch)
        {
            int bps = BitDepth / 8;
            int p = y * RowBytes + (x * Channels + ch) * bps;
            return bps == 2 ? (Pixels[p] << 8) | Pixels[p + 1] : Pixels[p];
        }

        /// <summary>First channel of every pixel, row-major, row 0 = top. Values 0..255 or 0..65535.</summary>
        public ushort[] Channel0()
        {
            var o = new ushort[Width * Height];
            int bps = BitDepth / 8;
            int stride = Channels * bps;
            for (int y = 0; y < Height; y++)
            {
                int p = y * RowBytes;
                int i = y * Width;
                if (bps == 2)
                    for (int x = 0; x < Width; x++, p += stride) o[i + x] = (ushort)((Pixels[p] << 8) | Pixels[p + 1]);
                else
                    for (int x = 0; x < Width; x++, p += stride) o[i + x] = Pixels[p];
            }
            return o;
        }
    }

    public static class Png
    {
        static readonly byte[] Signature = { 137, 80, 78, 71, 13, 10, 26, 10 };

        public static PngImage Load(string path)
        {
            return Decode(File.ReadAllBytes(path));
        }

        /// <summary>Reads only the IHDR (width, height, depth, colour type) without inflating.</summary>
        public static PngImage ReadHeader(byte[] data)
        {
            CheckSignature(data);
            if (ReadU32(data, 12) != 0x49484452) throw new InvalidDataException("PNG: first chunk is not IHDR");
            var img = new PngImage
            {
                Width = (int)ReadU32(data, 16),
                Height = (int)ReadU32(data, 20),
                BitDepth = data[24],
                ColorType = data[25],
            };
            if (data[26] != 0 || data[27] != 0) throw new InvalidDataException("PNG: unknown compression/filter method");
            if (data[28] != 0) throw new NotSupportedException("PNG: interlaced images are not supported");
            switch (img.ColorType)
            {
                case 0: img.Channels = 1; break;
                case 2: img.Channels = 3; break;
                case 4: img.Channels = 2; break;
                case 6: img.Channels = 4; break;
                default: throw new NotSupportedException("PNG: colour type " + img.ColorType + " is not supported (palette?)");
            }
            if (img.BitDepth != 8 && img.BitDepth != 16)
                throw new NotSupportedException("PNG: bit depth " + img.BitDepth + " is not supported");
            if (img.Width <= 0 || img.Height <= 0) throw new InvalidDataException("PNG: bad size");
            img.RowBytes = img.Width * img.Channels * (img.BitDepth / 8);
            return img;
        }

        public static PngImage Decode(byte[] data)
        {
            PngImage img = ReadHeader(data);

            // concatenate IDAT chunks
            var idat = new MemoryStream();
            int pos = 8;
            bool end = false;
            while (pos + 12 <= data.Length && !end)
            {
                int len = (int)ReadU32(data, pos);
                uint type = ReadU32(data, pos + 4);
                if (len < 0 || pos + 12 + len > data.Length) throw new InvalidDataException("PNG: truncated chunk");
                if (type == 0x49444154) idat.Write(data, pos + 8, len);          // IDAT
                else if (type == 0x49454E44) end = true;                       // IEND
                pos += 12 + len;
            }
            if (idat.Length < 3) throw new InvalidDataException("PNG: no image data");

            byte[] z = idat.ToArray();
            int cmf = z[0], flg = z[1];
            if ((cmf & 0x0F) != 8 || ((cmf << 8) | flg) % 31 != 0) throw new InvalidDataException("PNG: bad zlib header");
            if ((flg & 0x20) != 0) throw new InvalidDataException("PNG: zlib preset dictionary not supported");

            int bpp = Math.Max(1, img.Channels * img.BitDepth / 8);   // bytes per complete pixel
            int rb = img.RowBytes;
            var pixels = new byte[(long)rb * img.Height];
            var line = new byte[rb + 1];
            using (var ms = new MemoryStream(z, 2, z.Length - 2))
            using (var inflate = new DeflateStream(ms, CompressionMode.Decompress))
            {
                for (int y = 0; y < img.Height; y++)
                {
                    ReadFully(inflate, line, rb + 1);
                    int filter = line[0];
                    int o = y * rb;
                    int prev = o - rb;  // previous row offset in pixels[], < 0 for the first row
                    for (int i = 0; i < rb; i++)
                    {
                        int x = line[i + 1];
                        int a = i >= bpp ? pixels[o + i - bpp] : 0;
                        int b = prev >= 0 ? pixels[prev + i] : 0;
                        int c = (prev >= 0 && i >= bpp) ? pixels[prev + i - bpp] : 0;
                        int v;
                        switch (filter)
                        {
                            case 0: v = x; break;
                            case 1: v = x + a; break;
                            case 2: v = x + b; break;
                            case 3: v = x + ((a + b) >> 1); break;
                            case 4: v = x + Paeth(a, b, c); break;
                            default: throw new InvalidDataException("PNG: bad filter type " + filter + " in row " + y);
                        }
                        pixels[o + i] = (byte)v;
                    }
                }
            }
            img.Pixels = pixels;
            return img;
        }

        static int Paeth(int a, int b, int c)
        {
            int p = a + b - c;
            int pa = Math.Abs(p - a), pb = Math.Abs(p - b), pc = Math.Abs(p - c);
            if (pa <= pb && pa <= pc) return a;
            return pb <= pc ? b : c;
        }

        static void ReadFully(Stream s, byte[] buf, int count)
        {
            int got = 0;
            while (got < count)
            {
                int n = s.Read(buf, got, count - got);
                if (n <= 0) throw new InvalidDataException("PNG: image data ended early");
                got += n;
            }
        }

        static void CheckSignature(byte[] d)
        {
            if (d == null || d.Length < 33) throw new InvalidDataException("PNG: file too short");
            for (int i = 0; i < 8; i++)
                if (d[i] != Signature[i]) throw new InvalidDataException("not a PNG file");
        }

        static uint ReadU32(byte[] d, int p)
        {
            return ((uint)d[p] << 24) | ((uint)d[p + 1] << 16) | ((uint)d[p + 2] << 8) | d[p + 3];
        }
    }
}
