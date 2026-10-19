// meta.json model for UnityEngine.JsonUtility (field names must match the JSON keys exactly).
// Only the fields the importer needs are declared; JsonUtility ignores the rest.
using System;
using System.IO;
using UnityEngine;

namespace MarsMaker.UnityImport
{
    [Serializable]
    public class EastNorth
    {
        public double east;
        public double north;
    }

    [Serializable]
    public class SiteTile
    {
        public int x;                 // column
        public int y;                 // row (0 = north)
        public string height;         // h_{row}_{col}.png
        public string texture;        // t_{row}_{col}.png
        public string albedo;         // a_{row}_{col}.png (optional)
        public double valid_fraction;
        public EastNorth top_left_m;  // north-west corner of the tile in projection metres
        public double[] unity_position_m;
    }

    [Serializable]
    public class UnityBlock
    {
        public double[] terrain_size_m;  // [x, y, z]
        public double position_y_m;
    }

    [Serializable]
    public class SiteMeta
    {
        public string id;
        public string description;
        public string projection;
        public double center_lat;
        public double center_lon_east;
        public double metres_per_pixel;
        public int tile_px;
        public int tile_overlap_px;
        public double tile_size_m;
        public int[] grid_tiles;         // [cols, rows]
        public EastNorth grid_top_left_m;
        public double height_min_m;
        public double height_max_m;
        public double height_range_m;
        public double valid_fraction;
        public UnityBlock unity;
        public SiteTile[] tiles;

        public static SiteMeta Load(string siteDir)
        {
            string path = Path.Combine(siteDir, "meta.json");
            if (!File.Exists(path)) throw new FileNotFoundException("MarsMaker: no meta.json in " + siteDir, path);
            SiteMeta m = JsonUtility.FromJson<SiteMeta>(File.ReadAllText(path));
            if (m == null || m.tiles == null || m.tiles.Length == 0) throw new InvalidDataException("MarsMaker: meta.json has no tiles: " + path);
            if (m.height_range_m <= 0) throw new InvalidDataException("MarsMaker: meta.json has no height_range_m");
            if (m.metres_per_pixel <= 0) m.metres_per_pixel = 1.0;
            if (m.tile_size_m <= 0) m.tile_size_m = (m.tile_px - 1) * m.metres_per_pixel;
            return m;
        }

        /// <summary>Unity terrain height (size.y) and position.y. Uses the values the converter wrote in
        /// meta.json "unity" when present, else computes them (same formula).</summary>
        public void UnityHeightPlacement(out double sizeY, out double posY)
        {
            sizeY = MarsMaker.Core.Heightmap.UnityTerrainSizeY(height_range_m);
            posY = MarsMaker.Core.Heightmap.UnityPositionY(height_min_m, height_range_m);
            if (unity != null && unity.terrain_size_m != null && unity.terrain_size_m.Length == 3)
            {
                if (Math.Abs(unity.terrain_size_m[1] - sizeY) > 1e-6 || Math.Abs(unity.position_y_m - posY) > 1e-6)
                    Debug.LogWarning(string.Format("MarsMaker: meta.json unity block ({0}, {1}) differs from computed ({2}, {3}); using meta.json",
                        unity.terrain_size_m[1], unity.position_y_m, sizeY, posY));
                sizeY = unity.terrain_size_m[1];
                posY = unity.position_y_m;
            }
        }
    }
}
