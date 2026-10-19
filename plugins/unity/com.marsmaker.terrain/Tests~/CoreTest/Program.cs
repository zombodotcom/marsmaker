// Console test for the pure-C# MarsMaker core (PNG decoder, resampler, mesh builder).
// Not part of the Unity package build (folders ending in ~ are ignored by Unity).
// Run:  dotnet run -c Release --project CoreTest.csproj -- <site_dir> <out.json>
// then: python verify_core.py <site_dir> <out.json>   (compares against Pillow/numpy)
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Text;
using MarsMaker.Core;

static class Program
{
    static readonly int[][] Points = {
        new[]{0,0}, new[]{0,2016}, new[]{2016,0}, new[]{2016,2016}, new[]{1008,1008},
        new[]{17,1999}, new[]{500,1500}, new[]{1234,321}, new[]{2000,40}, new[]{777,777},
    };
    static readonly int[][] RPoints = {
        new[]{0,0}, new[]{0,1}, new[]{1,1}, new[]{1024,1024}, new[]{511,1700},
        new[]{2047,2048}, new[]{2048,2048}, new[]{333,999}, new[]{1500,77}, new[]{900,1900},
    };

    static string Key(string path)
    {
        string n = Path.GetFileNameWithoutExtension(path);
        string[] p = n.Split('_');
        int r, c;
        if (p.Length == 3 && int.TryParse(p[1], out r) && int.TryParse(p[2], out c))
            return p[0] + "_" + r.ToString("D4") + "_" + c.ToString("D4");
        return n;
    }

    static int Main(string[] args)
    {
        string site = args[0];
        var inv = CultureInfo.InvariantCulture;
        var sb = new StringBuilder();
        sb.Append("{\"tiles\":[");
        bool first = true;
        var sw = Stopwatch.StartNew();
        var files = new List<string>(Directory.GetFiles(site, "*.png"));
        // row-major (row, col) order so edge stitching sees north/west neighbours first
        files.Sort((x, y) => Key(x).CompareTo(Key(y)));
        var southRows = new Dictionary<string, ushort[]>();
        var eastCols = new Dictionary<string, ushort[]>();
        foreach (var path in files)
        {
            string name = Path.GetFileName(path);
            if (!(name.StartsWith("h_") || name.StartsWith("t_") || name.StartsWith("a_"))) continue;
            var t0 = sw.ElapsedMilliseconds;
            PngImage img = Png.Load(path);
            long ms = sw.ElapsedMilliseconds - t0;
            ushort[] v = img.Channel0();
            long sum = 0; int zeros = 0;
            foreach (var x in v) { sum += x; if (x == 0) zeros++; }
            if (!first) sb.Append(','); first = false;
            sb.AppendFormat(inv, "{{\"name\":\"{0}\",\"w\":{1},\"h\":{2},\"depth\":{3},\"ctype\":{4},\"sum\":{5},\"zeros\":{6},\"decode_ms\":{7},\"px\":[",
                name, img.Width, img.Height, img.BitDepth, img.ColorType, sum, zeros, ms);
            for (int i = 0; i < Points.Length; i++)
                sb.Append(i > 0 ? "," : "").Append(img.Sample(Points[i][1], Points[i][0], 0).ToString(inv));
            sb.Append(']');
            if (name.StartsWith("h_"))
            {
                string[] parts = Path.GetFileNameWithoutExtension(name).Split('_');
                int tr = int.Parse(parts[1]), tc = int.Parse(parts[2]);
                ushort[] nrow, wcol;
                southRows.TryGetValue((tr - 1) + "_" + tc, out nrow);
                eastCols.TryGetValue(tr + "_" + (tc - 1), out wcol);
                int stitched = Heightmap.StitchEdges(v, img.Width, img.Height, nrow, wcol);
                southRows[tr + "_" + tc] = Heightmap.Row(v, img.Width, img.Height - 1);
                eastCols[tr + "_" + tc] = Heightmap.Column(v, img.Width, img.Height, img.Width - 1);
                sb.AppendFormat(inv, ",\"stitched\":{0}", stitched);
                foreach (int n in new[] { 2049, 1025 })
                {
                    float[] g = Heightmap.Resample(v, img.Width, img.Height, n);
                    int nan = 0; foreach (var f in g) if (float.IsNaN(f)) nan++;
                    bool[] q = Heightmap.QuadValid(g, n);
                    int qv = 0; foreach (var b in q) if (b) qv++;
                    sb.AppendFormat(inv, ",\"r{0}\":{{\"nan\":{1},\"quads_valid\":{2},\"vals\":[", n, nan, qv);
                    for (int i = 0; i < RPoints.Length; i++)
                    {
                        int rr = Math.Min(RPoints[i][0], n - 1), cc = Math.Min(RPoints[i][1], n - 1);
                        float f = g[rr * n + cc];
                        sb.Append(i > 0 ? "," : "").Append(float.IsNaN(f) ? "null" : f.ToString("R", inv));
                    }
                    sb.Append("],\"edge_e\":[");
                    // east edge column and south edge row, to check against neighbours
                    for (int i = 0; i < n; i += 97) { float f = g[i * n + n - 1]; sb.Append(i > 0 ? "," : "").Append(float.IsNaN(f) ? "null" : f.ToString("R", inv)); }
                    sb.Append("],\"edge_w\":[");
                    for (int i = 0; i < n; i += 97) { float f = g[i * n]; sb.Append(i > 0 ? "," : "").Append(float.IsNaN(f) ? "null" : f.ToString("R", inv)); }
                    sb.Append("],\"edge_n\":[");
                    for (int i = 0; i < n; i += 97) { float f = g[i]; sb.Append(i > 0 ? "," : "").Append(float.IsNaN(f) ? "null" : f.ToString("R", inv)); }
                    sb.Append("],\"edge_s\":[");
                    for (int i = 0; i < n; i += 97) { float f = g[(n - 1) * n + i]; sb.Append(i > 0 ? "," : "").Append(float.IsNaN(f) ? "null" : f.ToString("R", inv)); }
                    int filled = Heightmap.FillNoData(g, n, n, 8);
                    int nan2 = 0; foreach (var f in g) if (float.IsNaN(f)) nan2++;
                    sb.AppendFormat(inv, "],\"filled\":{0},\"nan_after_fill\":{1}}}", filled, nan2);
                }
                TileMesh m = MeshBuilder.Build(v, img.Width, img.Height, 4, 57.27127456665039, 589.7566795349121, 1.0, 0.0);
                if (m != null)
                    sb.AppendFormat(inv, ",\"mesh4\":{{\"verts\":{0},\"tris\":{1},\"miny\":{2},\"maxy\":{3}}}",
                        m.VertexCount, m.Triangles.Length / 3, m.MinY.ToString("R", inv), m.MaxY.ToString("R", inv));
            }
            sb.Append('}');
            Console.WriteLine(name + " decoded in " + ms + " ms");
        }
        sb.Append("]}");
        File.WriteAllText(args[1], sb.ToString());
        // placement maths
        Console.WriteLine("sizeY " + Heightmap.UnityTerrainSizeY(589.7566795349121).ToString("R", inv) +
                          " posY " + Heightmap.UnityPositionY(57.27127456665039, 589.7566795349121).ToString("R", inv));
        Console.WriteLine("total " + sw.ElapsedMilliseconds + " ms");
        return 0;
    }
}
