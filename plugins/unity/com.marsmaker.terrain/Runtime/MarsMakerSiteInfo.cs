using UnityEngine;

namespace MarsMaker.UnityImport
{
    /// <summary>
    /// Added to the root object of an imported MarsMaker site. Holds the georeference so scripts can
    /// convert between Unity positions and the site's projection / Mars heights.
    /// Unity axes: +X = east, +Y = up, +Z = north, 1 unit = 1 m.
    /// </summary>
    [DisallowMultipleComponent]
    public class MarsMakerSiteInfo : MonoBehaviour
    {
        public string siteDir;
        public string id;
        [Tooltip("PROJ string of the site's local projection (meta.json \"projection\").")]
        public string projection;
        public double centerLat;
        public double centerLonEast;
        public double heightMinM;
        public double heightMaxM;
        [Tooltip("Subtracted from Mars heights (height_min_m when imported with Relative heights).")]
        public double yOffsetM;
        [Tooltip("Projection easting placed at local X = 0 (non-zero when imported with Center).")]
        public double originEastM;
        [Tooltip("Projection northing placed at local Z = 0 (non-zero when imported with Center).")]
        public double originNorthM;

        /// <summary>Local position (relative to this object) of a projection east/north and Mars height.</summary>
        public Vector3 ProjectionToLocal(double east, double north, double heightM)
        {
            return new Vector3((float)(east - originEastM), (float)(heightM - yOffsetM), (float)(north - originNorthM));
        }

        /// <summary>Inverse of ProjectionToLocal: returns (east, north, Mars height) in metres.</summary>
        public void LocalToProjection(Vector3 local, out double east, out double north, out double heightM)
        {
            east = local.x + originEastM;
            north = local.z + originNorthM;
            heightM = local.y + yOffsetM;
        }
    }
}
