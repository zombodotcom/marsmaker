// MarsMaker site importer for the Unity Editor: builds Unity Terrain objects (or plain meshes) from a
// MarsMaker site folder (meta.json + h_/t_/a_ tiles).
//
// Axes: 1 unit = 1 m, +X = east, +Y = up, +Z = north (Unity is left-handed, so east/up/north is the
// non-mirrored mapping; Godot/glTF use +Z = south because they are right-handed). The origin is the
// site's local projection centre (meta.json "projection"), or the middle of the tile grid with "Center".
using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using MarsMaker.Core;
using UnityEditor;
using UnityEngine;
using UnityEngine.Rendering;

namespace MarsMaker.UnityImport
{
    public enum ImportMode { Terrain = 0, Mesh = 1 }

    [Serializable]
    public class ImportOptions
    {
        public ImportMode mode = ImportMode.Terrain;
        /// <summary>Unity heightmap resolution: 1025 or 2049 (2^n + 1). The 2017-post tiles are resampled
        /// to this over the same 2016 m extent.</summary>
        public int heightmapResolution = 2049;
        /// <summary>Mesh mode: keep every Nth DTM post (1 = full 1 m detail, ~8M triangles per tile).</summary>
        public int meshStep = 4;
        public bool meshCollider = false;
        /// <summary>Site minimum height at Y = 0 instead of absolute Mars areoid height.</summary>
        public bool relativeHeights = false;
        /// <summary>Middle of the tile grid at X = Z = 0 instead of the projection centre.</summary>
        public bool center = false;
        public bool textures = true;
        /// <summary>Use a_{row}_{col}.png (approximate de-lit ortho) instead of t_ when present.</summary>
        public bool preferAlbedo = false;
        /// <summary>Multiply greyscale textures by tintColor (baked into the generated PNG).</summary>
        public bool tint = true;
        public Color tintColor = new Color(1.0f, 0.62f, 0.42f);
        /// <summary>Copy each tile's shared edge posts from its north/west neighbour so seams are exact.</summary>
        public bool stitchEdges = true;
        public float pixelError = 5f;
        /// <summary>Project folder for generated assets (TerrainData, layers, textures, meshes).</summary>
        public string assetFolder = "Assets/MarsMaker";
    }

    public static class MarsMakerImporter
    {
        public static readonly Color NoTextureColor = new Color(0.55f, 0.36f, 0.25f);

        public static GameObject Import(string siteDir, ImportOptions o)
        {
            siteDir = siteDir.Replace('\\', '/').TrimEnd('/');
            SiteMeta meta = SiteMeta.Load(siteDir);
            string site = Path.GetFileName(siteDir);
            if (o.heightmapResolution != 513 && o.heightmapResolution != 1025 && o.heightmapResolution != 2049 && o.heightmapResolution != 4097)
                throw new ArgumentException("heightmapResolution must be 2^n + 1 (513, 1025, 2049, 4097)");
            string folder = EnsureFolder(o.assetFolder.TrimEnd('/') + "/" + SafeName(site));

            double sizeY, posY;
            meta.UnityHeightPlacement(out sizeY, out posY);
            double yOff = o.relativeHeights ? meta.height_min_m : 0.0;
            double ox = 0, on = 0;   // origin in projection metres (east, north)
            if (o.center && meta.grid_tiles != null && meta.grid_tiles.Length == 2 && meta.grid_top_left_m != null)
            {
                ox = meta.grid_top_left_m.east + meta.grid_tiles[0] * meta.tile_size_m * 0.5;
                on = meta.grid_top_left_m.north - meta.grid_tiles[1] * meta.tile_size_m * 0.5;
            }

            var root = new GameObject(site);
            Undo.RegisterCreatedObjectUndo(root, "Import MarsMaker site");
            var info = root.AddComponent<MarsMakerSiteInfo>();
            info.siteDir = siteDir;
            info.id = meta.id;
            info.projection = meta.projection;
            info.centerLat = meta.center_lat;
            info.centerLonEast = meta.center_lon_east;
            info.heightMinM = meta.height_min_m;
            info.heightMaxM = meta.height_max_m;
            info.yOffsetM = yOff;
            info.originEastM = ox;
            info.originNorthM = on;

            var tiles = new List<SiteTile>(meta.tiles);
            tiles.Sort((a, b) => a.y != b.y ? a.y.CompareTo(b.y) : a.x.CompareTo(b.x));  // row-major for stitching
            var southRows = new Dictionary<long, ushort[]>();
            var eastCols = new Dictionary<long, ushort[]>();
            var terrains = new Dictionary<long, Terrain>();
            int groupId = Math.Abs(site.GetHashCode()) % 100000 + 1;
            var t0 = DateTime.Now;
            int built = 0;
            try
            {
                for (int i = 0; i < tiles.Count; i++)
                {
                    SiteTile t = tiles[i];
                    if (EditorUtility.DisplayCancelableProgressBar("MarsMaker", "Importing " + t.height + " (" + (i + 1) + "/" + tiles.Count + ")", i / (float)tiles.Count))
                        break;
                    string hpath = siteDir + "/" + t.height;
                    if (!File.Exists(hpath)) { Debug.LogWarning("MarsMaker: missing " + hpath); continue; }
                    PngImage png = Png.Load(hpath);
                    if (png.BitDepth != 16 || png.Channels != 1)
                        Debug.LogWarning("MarsMaker: " + t.height + " is not 16-bit greyscale (depth " + png.BitDepth + ", channels " + png.Channels + ")");
                    ushort[] raw = png.Channel0();
                    int w = png.Width, h = png.Height;
                    if (o.stitchEdges)
                    {
                        ushort[] nrow, wcol;
                        southRows.TryGetValue(Key(t.y - 1, t.x), out nrow);
                        eastCols.TryGetValue(Key(t.y, t.x - 1), out wcol);
                        Heightmap.StitchEdges(raw, w, h, nrow, wcol);
                        southRows[Key(t.y, t.x)] = Heightmap.Row(raw, w, h - 1);
                        eastCols[Key(t.y, t.x)] = Heightmap.Column(raw, w, h, w - 1);
                    }
                    string baseName = "h_" + t.y + "_" + t.x;
                    double tileW = (w - 1) * meta.metres_per_pixel, tileH = (h - 1) * meta.metres_per_pixel;
                    double east = t.top_left_m.east - ox, north = t.top_left_m.north - on;

                    Texture2D tex = null;
                    if (o.textures) tex = BuildTexture(siteDir, t, o, folder, baseName);

                    if (o.mode == ImportMode.Terrain)
                    {
                        Terrain terr = BuildTerrain(raw, w, h, o, folder, baseName, tex, tileW, tileH, sizeY);
                        if (terr == null) continue;
                        terr.transform.SetParent(root.transform, false);
                        // terrain origin = south-west corner
                        terr.transform.localPosition = new Vector3((float)east, (float)(posY - yOff), (float)(north - tileH));
                        terr.groupingID = groupId;
                        terr.allowAutoConnect = true;
                        terrains[Key(t.y, t.x)] = terr;
                    }
                    else
                    {
                        GameObject go = BuildMeshTile(raw, w, h, meta, o, folder, baseName, tex, yOff);
                        if (go == null) continue;
                        go.transform.SetParent(root.transform, false);
                        // mesh origin = north-west corner
                        go.transform.localPosition = new Vector3((float)east, 0f, (float)north);
                    }
                    built++;
                }
            }
            finally
            {
                EditorUtility.ClearProgressBar();
            }

            foreach (var kv in terrains)
            {
                int r = (int)(kv.Key >> 32), c = (int)(kv.Key & 0xFFFFFFFF);
                Terrain left, top, right, bottom;
                terrains.TryGetValue(Key(r, c - 1), out left);
                terrains.TryGetValue(Key(r - 1, c), out top);      // north = +Z = "top"
                terrains.TryGetValue(Key(r, c + 1), out right);
                terrains.TryGetValue(Key(r + 1, c), out bottom);
                kv.Value.SetNeighbors(left, top, right, bottom);
            }
            AssetDatabase.SaveAssets();
            Selection.activeGameObject = root;
            Debug.Log(string.Format("MarsMaker: imported {0}: {1} of {2} tiles as {3} in {4:F1} s (assets in {5})",
                site, built, tiles.Count, o.mode, (DateTime.Now - t0).TotalSeconds, folder));
            return root;
        }

        static long Key(int row, int col) { return ((long)row << 32) | (uint)col; }

        // ------------------------------------------------------------------------------------ terrain
        static Terrain BuildTerrain(ushort[] raw, int w, int h, ImportOptions o, string folder, string baseName,
                                    Texture2D tex, double tileW, double tileH, double sizeY)
        {
            int n = o.heightmapResolution;
            float[] g = Heightmap.Resample(raw, w, h, n);     // raw code units, row 0 = north, NaN = nodata
            bool[] quads = Heightmap.QuadValid(g, n);
            if (Heightmap.FillNoData(g, n, n, 8) < 0) return null;   // tile without any data

            // Unity: heights[z, x] with z = 0 at the SOUTH edge -> flip rows. Normalised height = v / 65535,
            // world height = position.y + normalised * size.y (sizeY = range * 65535/65534).
            var heights = new float[n, n];
            for (int r = 0; r < n; r++)
            {
                int z = n - 1 - r;
                int b = r * n;
                for (int c = 0; c < n; c++)
                    heights[z, c] = Mathf.Clamp01(g[b + c] / 65535f);
            }
            // holes: [z, x] over (n-1)^2 quads, true = surface present
            int m = n - 1;
            var holes = new bool[m, m];
            bool anyHole = false;
            for (int r = 0; r < m; r++)
            {
                int z = m - 1 - r;
                for (int c = 0; c < m; c++)
                {
                    bool ok = quads[r * m + c];
                    holes[z, c] = ok;
                    if (!ok) anyHole = true;
                }
            }

            var td = new TerrainData();
            td.heightmapResolution = n;                       // set before size (changing it rescales size)
            td.size = new Vector3((float)tileW, (float)sizeY, (float)tileH);
            td.SetHeights(0, 0, heights);
            if (anyHole) td.SetHoles(0, 0, holes);

            // one terrain layer covering the whole tile once (or a flat Mars colour without texture)
            var layer = new TerrainLayer();
            if (tex != null)
            {
                layer.diffuseTexture = tex;
                layer.tileSize = new Vector2((float)tileW, (float)tileH);
            }
            else
            {
                layer.diffuseTexture = SolidTexture(folder, NoTextureColor);
                layer.tileSize = new Vector2(16f, 16f);
            }
            layer.tileOffset = Vector2.zero;
            layer.metallic = 0f;
            layer.smoothness = 0.05f;
            AssetDatabase.CreateAsset(layer, folder + "/" + baseName + "_layer.terrainlayer");
            td.alphamapResolution = 16;                       // minimum; a single layer needs no detail
            td.terrainLayers = new[] { layer };
            var alpha = new float[16, 16, 1];
            for (int y = 0; y < 16; y++) for (int x = 0; x < 16; x++) alpha[y, x, 0] = 1f;
            td.SetAlphamaps(0, 0, alpha);
            AssetDatabase.CreateAsset(td, folder + "/" + baseName + "_terrain.asset");

            GameObject go = Terrain.CreateTerrainGameObject(td);
            go.name = baseName;
            var terr = go.GetComponent<Terrain>();
            terr.heightmapPixelError = o.pixelError;
            Material tm = DefaultTerrainMaterial();
            if (tm != null) terr.materialTemplate = tm;
            return terr;
        }

        // ------------------------------------------------------------------------------------ mesh
        static GameObject BuildMeshTile(ushort[] raw, int w, int h, SiteMeta meta, ImportOptions o, string folder,
                                        string baseName, Texture2D tex, double yOff)
        {
            TileMesh tm = MeshBuilder.Build(raw, w, h, o.meshStep, meta.height_min_m, meta.height_range_m, meta.metres_per_pixel, yOff);
            if (tm == null) return null;
            int nv = tm.VertexCount;
            var verts = new Vector3[nv];
            var norms = new Vector3[nv];
            var uvs = new Vector2[nv];
            for (int i = 0; i < nv; i++)
            {
                verts[i] = new Vector3(tm.Positions[i * 3], tm.Positions[i * 3 + 1], tm.Positions[i * 3 + 2]);
                norms[i] = new Vector3(tm.Normals[i * 3], tm.Normals[i * 3 + 1], tm.Normals[i * 3 + 2]);
                uvs[i] = new Vector2(tm.UVs[i * 2], tm.UVs[i * 2 + 1]);
            }
            var mesh = new Mesh();
            mesh.name = baseName;
            mesh.indexFormat = IndexFormat.UInt32;
            mesh.vertices = verts;
            mesh.normals = norms;
            mesh.uv = uvs;
            mesh.SetTriangles(tm.Triangles, 0);
            mesh.RecalculateBounds();
            AssetDatabase.CreateAsset(mesh, folder + "/" + baseName + "_mesh.asset");

            var go = new GameObject(baseName);
            go.AddComponent<MeshFilter>().sharedMesh = mesh;
            var mr = go.AddComponent<MeshRenderer>();
            mr.sharedMaterial = MeshMaterial(folder, baseName, tex);
            if (o.meshCollider) go.AddComponent<MeshCollider>().sharedMesh = mesh;
            return go;
        }

        static Material MeshMaterial(string folder, string baseName, Texture2D tex)
        {
            Material def = RenderPipelineMaterial("defaultMaterial");
            Shader sh = def != null ? def.shader : null;
            if (sh == null) sh = Shader.Find("Universal Render Pipeline/Lit");
            if (sh == null || GraphicsSettings.currentRenderPipeline == null) sh = Shader.Find("Standard");
            var mat = new Material(sh);
            mat.name = baseName;
            if (tex != null) { mat.mainTexture = tex; mat.color = Color.white; }
            else mat.color = NoTextureColor;
            foreach (string p in new[] { "_Smoothness", "_Glossiness" }) if (mat.HasProperty(p)) mat.SetFloat(p, 0.05f);
            if (mat.HasProperty("_Metallic")) mat.SetFloat("_Metallic", 0f);
            AssetDatabase.CreateAsset(mat, folder + "/" + baseName + "_mat.mat");
            return mat;
        }

        // ------------------------------------------------------------------------------------ textures
        /// <summary>Decodes t_ (or a_) and writes a tinted RGB PNG into the project. Returns null when no
        /// texture exists or it is all zero (converter writes that when ortho alignment failed).</summary>
        static Texture2D BuildTexture(string siteDir, SiteTile t, ImportOptions o, string folder, string baseName)
        {
            var cands = new List<string>();
            if (o.preferAlbedo)
                cands.Add(!string.IsNullOrEmpty(t.albedo) ? t.albedo : "a_" + t.y + "_" + t.x + ".png");
            cands.Add(!string.IsNullOrEmpty(t.texture) ? t.texture : "t_" + t.y + "_" + t.x + ".png");
            foreach (string name in cands)
            {
                string p = siteDir + "/" + name;
                if (!File.Exists(p)) continue;
                PngImage img = Png.Load(p);
                int w = img.Width, h = img.Height;
                bool grey = img.Channels <= 2;
                int shift = img.BitDepth == 16 ? 8 : 0;
                var pix = new Color32[w * h];
                bool any = false;
                Color tc = (grey && o.tint) ? o.tintColor : Color.white;
                for (int y = 0; y < h; y++)
                {
                    int dst = (h - 1 - y) * w;   // Texture2D row 0 = bottom
                    for (int x = 0; x < w; x++)
                    {
                        int r, g, b;
                        if (grey) { r = g = b = img.Sample(x, y, 0) >> shift; }
                        else { r = img.Sample(x, y, 0) >> shift; g = img.Sample(x, y, 1) >> shift; b = img.Sample(x, y, 2) >> shift; }
                        if (r != 0 || g != 0 || b != 0) any = true;
                        // multiplying sRGB-encoded values ~= multiplying linear values (power-law gamma)
                        pix[dst + x] = new Color32((byte)Mathf.RoundToInt(r * tc.r), (byte)Mathf.RoundToInt(g * tc.g), (byte)Mathf.RoundToInt(b * tc.b), 255);
                    }
                }
                if (!any) { Debug.LogWarning("MarsMaker: " + name + " is empty (all nodata); using a flat colour"); return null; }
                var tex = new Texture2D(w, h, TextureFormat.RGB24, false);
                tex.SetPixels32(pix);
                tex.Apply(false);
                byte[] bytes = tex.EncodeToPNG();
                UnityEngine.Object.DestroyImmediate(tex);
                string assetPath = folder + "/" + baseName + "_color.png";
                File.WriteAllBytes(FullPath(assetPath), bytes);
                AssetDatabase.ImportAsset(assetPath, ImportAssetOptions.ForceSynchronousImport);
                var imp = AssetImporter.GetAtPath(assetPath) as TextureImporter;
                if (imp != null)
                {
                    imp.textureType = TextureImporterType.Default;
                    imp.sRGBTexture = true;
                    imp.wrapMode = TextureWrapMode.Clamp;
                    imp.mipmapEnabled = true;
                    imp.anisoLevel = 4;
                    imp.maxTextureSize = 4096;
                    imp.SaveAndReimport();
                }
                return AssetDatabase.LoadAssetAtPath<Texture2D>(assetPath);
            }
            return null;
        }

        static Texture2D SolidTexture(string folder, Color c)
        {
            string assetPath = folder + "/flat_color.png";
            var existing = AssetDatabase.LoadAssetAtPath<Texture2D>(assetPath);
            if (existing != null) return existing;
            var tex = new Texture2D(4, 4, TextureFormat.RGB24, false);
            var px = new Color[16];
            for (int i = 0; i < 16; i++) px[i] = c;
            tex.SetPixels(px);
            tex.Apply();
            File.WriteAllBytes(FullPath(assetPath), tex.EncodeToPNG());
            UnityEngine.Object.DestroyImmediate(tex);
            AssetDatabase.ImportAsset(assetPath, ImportAssetOptions.ForceSynchronousImport);
            return AssetDatabase.LoadAssetAtPath<Texture2D>(assetPath);
        }

        // ------------------------------------------------------------------------------------ helpers
        /// <summary>Default terrain material of the active render pipeline (URP/HDRP), or the built-in one.
        /// Looked up by reflection so the code compiles on 2022.3 and Unity 6 alike.</summary>
        static Material DefaultTerrainMaterial()
        {
            Material m = RenderPipelineMaterial("defaultTerrainMaterial");
            if (m != null) return m;
            if (GraphicsSettings.currentRenderPipeline != null) return null;   // let the pipeline decide
            try { return AssetDatabase.GetBuiltinExtraResource<Material>("Default-Terrain-Standard.mat"); }
            catch { return null; }
        }

        static Material RenderPipelineMaterial(string property)
        {
            RenderPipelineAsset rp = GraphicsSettings.currentRenderPipeline;
            if (rp == null) return null;
            try
            {
                PropertyInfo p = rp.GetType().GetProperty(property, BindingFlags.Public | BindingFlags.Instance);
                return p != null ? p.GetValue(rp, null) as Material : null;
            }
            catch { return null; }
        }

        static string FullPath(string assetPath)
        {
            return Path.Combine(Directory.GetParent(Application.dataPath).FullName, assetPath);
        }

        static string SafeName(string s)
        {
            foreach (char ch in Path.GetInvalidFileNameChars()) s = s.Replace(ch, '_');
            return s;
        }

        /// <summary>Creates "Assets/a/b/c" folder by folder; returns the asset path.</summary>
        static string EnsureFolder(string assetPath)
        {
            assetPath = assetPath.Replace('\\', '/');
            if (!assetPath.StartsWith("Assets")) throw new ArgumentException("asset folder must be under Assets/: " + assetPath);
            string[] parts = assetPath.Split('/');
            string cur = parts[0];
            for (int i = 1; i < parts.Length; i++)
            {
                if (parts[i].Length == 0) continue;
                string next = cur + "/" + parts[i];
                if (!AssetDatabase.IsValidFolder(next)) AssetDatabase.CreateFolder(cur, parts[i]);
                cur = next;
            }
            return cur;
        }
    }
}
