using System;
using System.IO;
using UnityEditor;
using UnityEngine;

namespace MarsMaker.UnityImport
{
    /// <summary>Tools > MarsMaker > Import Site</summary>
    public class MarsMakerImportWindow : EditorWindow
    {
        const string PrefKey = "MarsMaker.Import.";
        static readonly int[] Resolutions = { 1025, 2049 };
        static readonly string[] ResolutionLabels = { "1025 (about 1.97 m posts, downsampled)", "2049 (about 0.98 m posts, upsampled)" };

        string siteDir = "";
        ImportOptions opts = new ImportOptions();
        SiteMeta meta;
        string metaError;
        Vector2 scroll;

        [MenuItem("Tools/MarsMaker/Import Site")]
        public static void Open()
        {
            var w = GetWindow<MarsMakerImportWindow>(false, "MarsMaker Import", true);
            w.minSize = new Vector2(420, 420);
            w.Show();
        }

        void OnEnable()
        {
            siteDir = EditorPrefs.GetString(PrefKey + "siteDir", "");
            string json = EditorPrefs.GetString(PrefKey + "options", "");
            if (!string.IsNullOrEmpty(json))
            {
                try { EditorJsonUtility.FromJsonOverwrite(json, opts); } catch { opts = new ImportOptions(); }
            }
            ReadMeta();
        }

        void OnDisable()
        {
            EditorPrefs.SetString(PrefKey + "siteDir", siteDir);
            EditorPrefs.SetString(PrefKey + "options", EditorJsonUtility.ToJson(opts));
        }

        void ReadMeta()
        {
            meta = null;
            metaError = null;
            if (string.IsNullOrEmpty(siteDir)) return;
            try { meta = SiteMeta.Load(siteDir); }
            catch (Exception e) { metaError = e.Message; }
        }

        void OnGUI()
        {
            scroll = EditorGUILayout.BeginScrollView(scroll);
            EditorGUILayout.LabelField("Site folder (contains meta.json)", EditorStyles.boldLabel);
            EditorGUILayout.BeginHorizontal();
            EditorGUI.BeginChangeCheck();
            siteDir = EditorGUILayout.TextField(siteDir);
            if (EditorGUI.EndChangeCheck()) ReadMeta();
            if (GUILayout.Button("Browse...", GUILayout.Width(80)))
            {
                string start = Directory.Exists(siteDir) ? siteDir : "";
                string p = EditorUtility.OpenFolderPanel("MarsMaker site folder", start, "");
                if (!string.IsNullOrEmpty(p)) { siteDir = p; ReadMeta(); GUI.FocusControl(null); }
            }
            EditorGUILayout.EndHorizontal();

            if (metaError != null) EditorGUILayout.HelpBox(metaError, MessageType.Error);
            else if (meta != null)
            {
                int cols = meta.grid_tiles != null && meta.grid_tiles.Length == 2 ? meta.grid_tiles[0] : 0;
                int rows = meta.grid_tiles != null && meta.grid_tiles.Length == 2 ? meta.grid_tiles[1] : 0;
                EditorGUILayout.HelpBox(string.Format("{0}\n{1} tiles (grid {2} x {3}), {4} px / {5} m per tile\nheights {6:F1} .. {7:F1} m, centre {8:F4}, {9:F4} E",
                    meta.id, meta.tiles.Length, cols, rows, meta.tile_px, meta.tile_size_m,
                    meta.height_min_m, meta.height_max_m, meta.center_lat, meta.center_lon_east), MessageType.Info);
            }

            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Options", EditorStyles.boldLabel);
            opts.mode = (ImportMode)EditorGUILayout.EnumPopup(new GUIContent("Import as", "Terrain: one Unity Terrain per tile (LOD, holes, TerrainCollider). Mesh: one MeshRenderer per tile."), opts.mode);
            if (opts.mode == ImportMode.Terrain)
            {
                int idx = Math.Max(0, Array.IndexOf(Resolutions, opts.heightmapResolution));
                idx = EditorGUILayout.Popup(new GUIContent("Heightmap resolution", "Unity needs 2^n+1. The 2017-post MarsMaker tiles are resampled over the same 2016 m extent."), idx, ResolutionLabels);
                opts.heightmapResolution = Resolutions[idx];
                opts.pixelError = EditorGUILayout.Slider(new GUIContent("Pixel error", "Terrain LOD tolerance (Unity default 5)"), opts.pixelError, 1f, 50f);
            }
            else
            {
                opts.meshStep = EditorGUILayout.IntSlider(new GUIContent("Mesh step", "Keep every Nth DTM post. 1 = full 1 m detail (~8M triangles per tile, heavy)."), opts.meshStep, 1, 64);
                opts.meshCollider = EditorGUILayout.Toggle("Mesh collider", opts.meshCollider);
            }
            opts.relativeHeights = EditorGUILayout.Toggle(new GUIContent("Relative heights", "Site minimum at Y = 0 instead of absolute Mars areoid height"), opts.relativeHeights);
            opts.center = EditorGUILayout.Toggle(new GUIContent("Center", "Middle of the tile grid at X = Z = 0 instead of the projection centre"), opts.center);
            opts.stitchEdges = EditorGUILayout.Toggle(new GUIContent("Stitch tile edges", "Copy shared edge posts from the north/west neighbour so seams are exact"), opts.stitchEdges);
            opts.textures = EditorGUILayout.Toggle("Textures", opts.textures);
            using (new EditorGUI.DisabledScope(!opts.textures))
            {
                opts.preferAlbedo = EditorGUILayout.Toggle(new GUIContent("Prefer de-lit albedo (a_)", "Use a_{row}_{col}.png (approximate, sun shading removed) when it exists; better under engine lighting"), opts.preferAlbedo);
                opts.tint = EditorGUILayout.Toggle(new GUIContent("Mars tint", "Multiply the greyscale HiRISE image by a dust colour (baked into the generated PNG)"), opts.tint);
                using (new EditorGUI.DisabledScope(!opts.tint))
                    opts.tintColor = EditorGUILayout.ColorField("Tint colour", opts.tintColor);
            }
            opts.assetFolder = EditorGUILayout.TextField(new GUIContent("Asset folder", "Generated TerrainData/layers/textures/meshes go to <folder>/<site>/"), opts.assetFolder);

            EditorGUILayout.Space();
            using (new EditorGUI.DisabledScope(meta == null))
            {
                if (GUILayout.Button("Import", GUILayout.Height(30)))
                {
                    try { MarsMakerImporter.Import(siteDir, opts); }
                    catch (Exception e) { Debug.LogException(e); EditorUtility.DisplayDialog("MarsMaker", "Import failed: " + e.Message, "OK"); }
                    GUIUtility.ExitGUI();
                }
            }
            EditorGUILayout.HelpBox("Axes: X = east, Y = up, Z = north, 1 unit = 1 m. Assets are written to "
                + opts.assetFolder + "/<site>/ and overwritten on re-import.", MessageType.None);
            EditorGUILayout.EndScrollView();
        }
    }
}
