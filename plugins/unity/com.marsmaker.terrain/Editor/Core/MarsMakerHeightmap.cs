// MarsMaker heightmap maths: pure C# (no UnityEngine) so it can be tested with plain `dotnet`.
//
// Conventions in this file: grids are row-major with row 0 = NORTH (the PNG's top row), column 0 = WEST.
// Height samples are kept in "raw" units: the MarsMaker uint16 code v (1..65535), float.NaN = nodata.
// Metres: h = height_min_m + (v - 1) * height_range_m / 65534.
using System;
using System.Collections.Generic;

namespace MarsMaker.Core
{
    public static class Heightmap
    {
        /// <summary>Unity terrain size Y such that height = positionY + (v / 65535) * sizeY reproduces the
        /// MarsMaker decoding exactly: sizeY = range * 65535 / 65534.</summary>
        public static double UnityTerrainSizeY(double rangeM) { return rangeM * 65535.0 / 65534.0; }

        /// <summary>Unity terrain position Y (v = 0 level): height_min - range / 65534.</summary>
        public static double UnityPositionY(double minM, double rangeM) { return minM - rangeM / 65534.0; }

        public static double Metres(double v, double minM, double rangeM) { return minM + (v - 1.0) * rangeM / 65534.0; }

        /// <summary>
        /// Resamples a w x h uint16 grid (0 = nodata) to an n x n grid spanning the SAME extent: output
        /// sample i sits at source position i * (w - 1) / (n - 1), so the first/last rows and columns map
        /// exactly onto the tile edges and neighbouring tiles (which share their edge posts) still match.
        /// Upsampling (n >= w) is bilinear. Downsampling uses a separable tent filter whose radius is the
        /// sample spacing (bilinear generalised; reduces aliasing), except on the outer edge rows/columns,
        /// which are filtered only along the edge so both neighbours compute identical edge values.
        /// A sample is nodata when any source post of its bilinear cell is nodata; otherwise the value is
        /// the filter average over valid posts. Returns raw units (float), NaN = nodata.
        /// </summary>
        public static float[] Resample(ushort[] src, int w, int h, int n)
        {
            if (src.Length != w * h) throw new ArgumentException("size mismatch");
            if (n < 2) throw new ArgumentException("n must be >= 2");
            var dst = new float[n * n];
            if (n == w && n == h)
            {
                for (int i = 0; i < dst.Length; i++) dst[i] = src[i] == 0 ? float.NaN : src[i];
                return dst;
            }
            Axis[] ax = BuildAxis(w, n);
            Axis[] ay = BuildAxis(h, n);
            for (int r = 0; r < n; r++)
            {
                Axis yr = ay[r];
                for (int c = 0; c < n; c++)
                {
                    Axis xc = ax[c];
                    // validity: the bilinear support cell
                    bool ok = true;
                    for (int yy = yr.B0; yy <= yr.B1 && ok; yy++)
                        for (int xx = xc.B0; xx <= xc.B1; xx++)
                            if (src[yy * w + xx] == 0) { ok = false; break; }
                    if (!ok) { dst[r * n + c] = float.NaN; continue; }
                    double sum = 0, wsum = 0;
                    for (int j = 0; j < yr.W.Length; j++)
                    {
                        int row = (yr.Start + j) * w;
                        double wy = yr.W[j];
                        for (int i = 0; i < xc.W.Length; i++)
                        {
                            ushort v = src[row + xc.Start + i];
                            if (v == 0) continue;
                            double wt = wy * xc.W[i];
                            sum += wt * v;
                            wsum += wt;
                        }
                    }
                    dst[r * n + c] = (float)(sum / wsum);
                }
            }
            return dst;
        }

        struct Axis
        {
            public int Start;     // first source index of the filter window
            public double[] W;    // filter weights
            public int B0, B1;    // bilinear support (inclusive)
        }

        static Axis[] BuildAxis(int srcN, int n)
        {
            var res = new Axis[n];
            double scale = (srcN - 1) / (double)(n - 1);
            double radius = Math.Max(1.0, scale);
            for (int i = 0; i < n; i++)
            {
                double s = i * scale;
                if (i == n - 1) s = srcN - 1;          // exact, no rounding drift
                var a = new Axis();
                int f = (int)Math.Floor(s);
                if (f >= srcN - 1) f = srcN - 1;
                double fr = s - f;
                a.B0 = f;
                a.B1 = fr > 1e-9 ? Math.Min(f + 1, srcN - 1) : f;
                bool edge = (i == 0 || i == n - 1);
                if (edge)
                {
                    a.Start = f; a.W = new[] { 1.0 };
                }
                else
                {
                    int lo = Math.Max(0, (int)Math.Ceiling(s - radius + 1e-9));
                    int hi = Math.Min(srcN - 1, (int)Math.Floor(s + radius - 1e-9));
                    if (hi < lo) { lo = hi = f; }
                    var w = new double[hi - lo + 1];
                    for (int k = lo; k <= hi; k++) w[k - lo] = Math.Max(0.0, 1.0 - Math.Abs(k - s) / radius);
                    // a sample exactly on a source post with radius 1 has weight 0 neighbours: fine
                    a.Start = lo; a.W = w;
                }
                res[i] = a;
            }
            return res;
        }

        /// <summary>Quad (cell) validity for an n x n grid: (n-1) x (n-1), row 0 = northmost quad.
        /// A quad is valid when all 4 corner samples have data (same rule as the Blender/Godot importers).</summary>
        public static bool[] QuadValid(float[] g, int n)
        {
            int m = n - 1;
            var q = new bool[m * m];
            for (int r = 0; r < m; r++)
                for (int c = 0; c < m; c++)
                {
                    int a = r * n + c;
                    q[r * m + c] = !(float.IsNaN(g[a]) || float.IsNaN(g[a + 1]) || float.IsNaN(g[a + n]) || float.IsNaN(g[a + n + 1]));
                }
            return q;
        }

        /// <summary>
        /// Replaces NaN samples in place: `passes` rounds of growing the valid area by one sample (each new
        /// sample = mean of its valid 4-neighbours), then any remaining NaN = the minimum valid value.
        /// The filled values are hidden by holes; they only keep coarse terrain LODs and normals near the
        /// footprint edge plausible. Returns the number of filled samples, or -1 if the grid has no data.
        /// </summary>
        public static int FillNoData(float[] g, int w, int h, int passes)
        {
            float lo = float.PositiveInfinity;
            int missing = 0;
            for (int i = 0; i < g.Length; i++)
            {
                if (float.IsNaN(g[i])) missing++;
                else if (g[i] < lo) lo = g[i];
            }
            if (missing == g.Length) return -1;
            if (missing == 0) return 0;
            int filled = missing;
            var frontier = new List<int>();
            var next = new List<float>();
            for (int p = 0; p < passes && missing > 0; p++)
            {
                frontier.Clear(); next.Clear();
                for (int r = 0; r < h; r++)
                    for (int c = 0; c < w; c++)
                    {
                        int i = r * w + c;
                        if (!float.IsNaN(g[i])) continue;
                        double s = 0; int k = 0;
                        if (c > 0 && !float.IsNaN(g[i - 1])) { s += g[i - 1]; k++; }
                        if (c < w - 1 && !float.IsNaN(g[i + 1])) { s += g[i + 1]; k++; }
                        if (r > 0 && !float.IsNaN(g[i - w])) { s += g[i - w]; k++; }
                        if (r < h - 1 && !float.IsNaN(g[i + w])) { s += g[i + w]; k++; }
                        if (k > 0) { frontier.Add(i); next.Add((float)(s / k)); }
                    }
                if (frontier.Count == 0) break;
                for (int j = 0; j < frontier.Count; j++) g[frontier[j]] = next[j];
                missing -= frontier.Count;
            }
            if (missing > 0)
                for (int i = 0; i < g.Length; i++) if (float.IsNaN(g[i])) g[i] = lo;
            return filled;
        }

        /// <summary>
        /// Neighbouring tiles share their edge posts (1 px overlap), but in some converted sites a few shared
        /// posts differ by 1 code (~range/65534, under 1 cm). To make seams bit-identical, overwrite this
        /// tile's north row with the north neighbour's south row and its west column with the west
        /// neighbour's east column (either may be null). Returns the number of posts changed.
        /// </summary>
        public static int StitchEdges(ushort[] raw, int w, int h, ushort[] northNeighbourSouthRow, ushort[] westNeighbourEastCol)
        {
            int changed = 0;
            if (northNeighbourSouthRow != null && northNeighbourSouthRow.Length == w)
                for (int c = 0; c < w; c++)
                    if (raw[c] != northNeighbourSouthRow[c]) { raw[c] = northNeighbourSouthRow[c]; changed++; }
            if (westNeighbourEastCol != null && westNeighbourEastCol.Length == h)
                for (int r = 0; r < h; r++)
                    if (raw[r * w] != westNeighbourEastCol[r]) { raw[r * w] = westNeighbourEastCol[r]; changed++; }
            return changed;
        }

        public static ushort[] Row(ushort[] raw, int w, int r)
        {
            var o = new ushort[w];
            Array.Copy(raw, r * w, o, 0, w);
            return o;
        }

        public static ushort[] Column(ushort[] raw, int w, int h, int c)
        {
            var o = new ushort[h];
            for (int r = 0; r < h; r++) o[r] = raw[r * w + c];
            return o;
        }

        /// <summary>Sample positions 0, step, 2*step ... plus the last index (tile edges always meet).</summary>
        public static int[] SampleIndices(int nPx, int step)
        {
            step = Math.Max(1, step);
            var l = new List<int>();
            for (int i = 0; i < nPx; i += step) l.Add(i);
            if (l[l.Count - 1] != nPx - 1) l.Add(nPx - 1);
            return l.ToArray();
        }
    }

    /// <summary>Engine-neutral triangle mesh of one tile (Unity axes: X = east, Y = up, Z = north).</summary>
    public sealed class TileMesh
    {
        public float[] Positions;  // xyz, relative to the tile's north-west corner (so z <= 0)
        public float[] Normals;    // xyz
        public float[] UVs;        // uv, v = 0 at the south edge (Unity texture convention)
        public int[] Triangles;    // clockwise seen from above (Unity front face)
        public float MinY, MaxY;
        public int VertexCount { get { return Positions.Length / 3; } }
    }

    public static class MeshBuilder
    {
        /// <summary>Port of the Godot importer's build_mesh with Unity axes. raw: w x h uint16, row 0 = north.
        /// Quads with any nodata corner are dropped. Returns null when the tile has no complete quad.</summary>
        public static TileMesh Build(ushort[] raw, int w, int h, int step, double hmin, double range,
                                     double mpp, double yOffset)
        {
            int[] ri = Heightmap.SampleIndices(h, step);
            int[] ci = Heightmap.SampleIndices(w, step);
            int nr = ri.Length, nc = ci.Length;
            double k = range / 65534.0;
            var hs = new float[nr * nc];
            for (int r = 0; r < nr; r++)
                for (int c = 0; c < nc; c++)
                {
                    int v = raw[ri[r] * w + ci[c]];
                    hs[r * nc + c] = v > 0 ? (float)(hmin + (v - 1) * k - yOffset) : float.NaN;
                }
            var qok = new bool[(nr - 1) * (nc - 1)];
            var used = new bool[nr * nc];
            int nq = 0;
            for (int r = 0; r < nr - 1; r++)
                for (int c = 0; c < nc - 1; c++)
                {
                    int a = r * nc + c;
                    if (float.IsNaN(hs[a]) || float.IsNaN(hs[a + 1]) || float.IsNaN(hs[a + nc]) || float.IsNaN(hs[a + nc + 1])) continue;
                    qok[r * (nc - 1) + c] = true;
                    used[a] = used[a + 1] = used[a + nc] = used[a + nc + 1] = true;
                    nq++;
                }
            if (nq == 0) return null;

            int nv = 0;
            for (int i = 0; i < used.Length; i++) if (used[i]) nv++;
            var m = new TileMesh
            {
                Positions = new float[nv * 3], Normals = new float[nv * 3], UVs = new float[nv * 2],
                Triangles = new int[nq * 6], MinY = float.PositiveInfinity, MaxY = float.NegativeInfinity,
            };
            var vid = new int[nr * nc];
            int vi = 0;
            for (int r = 0; r < nr; r++)
            {
                int rn = Math.Max(r - 1, 0), rs = Math.Min(r + 1, nr - 1);
                double dz = (ri[rs] - ri[rn]) * mpp;
                float vv = 1f - (float)((ri[r] + 0.5) / h);
                for (int c = 0; c < nc; c++)
                {
                    int a = r * nc + c;
                    if (!used[a]) { vid[a] = -1; continue; }
                    vid[a] = vi;
                    float y = hs[a];
                    if (y < m.MinY) m.MinY = y;
                    if (y > m.MaxY) m.MaxY = y;
                    m.Positions[vi * 3] = (float)(ci[c] * mpp);
                    m.Positions[vi * 3 + 1] = y;
                    m.Positions[vi * 3 + 2] = (float)(-ri[r] * mpp);
                    m.UVs[vi * 2] = (float)((ci[c] + 0.5) / w);
                    m.UVs[vi * 2 + 1] = vv;
                    int cw = Math.Max(c - 1, 0), ce = Math.Min(c + 1, nc - 1);
                    float hw = hs[r * nc + cw], he = hs[r * nc + ce], hn = hs[rn * nc + c], hsth = hs[rs * nc + c];
                    if (float.IsNaN(hw)) hw = y;
                    if (float.IsNaN(he)) he = y;
                    if (float.IsNaN(hn)) hn = y;
                    if (float.IsNaN(hsth)) hsth = y;
                    double dx = (ci[ce] - ci[cw]) * mpp;
                    // +Z = north: dh/dz = (hn - hs) / dz
                    double nx = -(he - hw) / dx, ny = 1.0, nz = -(hn - hsth) / dz;
                    double len = Math.Sqrt(nx * nx + ny * ny + nz * nz);
                    m.Normals[vi * 3] = (float)(nx / len);
                    m.Normals[vi * 3 + 1] = (float)(ny / len);
                    m.Normals[vi * 3 + 2] = (float)(nz / len);
                    vi++;
                }
            }
            // Unity: front faces are clockwise seen from the front (+Y). Seen from above X = right (east),
            // Z = up (north), so NW -> NE -> SE is clockwise.
            int j = 0;
            for (int r = 0; r < nr - 1; r++)
                for (int c = 0; c < nc - 1; c++)
                {
                    if (!qok[r * (nc - 1) + c]) continue;
                    int a = r * nc + c;
                    int nw = vid[a], ne = vid[a + 1], sw = vid[a + nc], se = vid[a + nc + 1];
                    m.Triangles[j++] = nw; m.Triangles[j++] = ne; m.Triangles[j++] = se;
                    m.Triangles[j++] = nw; m.Triangles[j++] = se; m.Triangles[j++] = sw;
                }
            return m;
        }
    }
}
