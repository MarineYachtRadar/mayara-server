//! Land from a GeoJSON file, so the emulator paints the coast a chart shows
//! around the boat it follows.

use std::f64::consts::TAU;
use std::path::Path;

use serde_json::Value;

use crate::radar::GeoPosition;

const METERS_PER_DEGREE_LAT: f64 = 111_320.0;
const HALF_LONGITUDE_DEGREES: f64 = 180.0;
const FULL_LONGITUDE_DEGREES: f64 = 360.0;

/// How far into land the radar sees from where its beam reaches the coast,
/// in meters. Low land behind the shore line returns little.
pub(crate) const LAND_DEPTH: f64 = 400.0;

/// The land areas of a GeoJSON file: the rings of its polygons, each a
/// closed list of (longitude, latitude) points. Holes are rings too; a
/// point is on land when a line from it crosses an odd number of edges.
pub(crate) struct Coast {
    rings: Vec<Vec<(f64, f64)>>,
}

impl Coast {
    pub(crate) fn load(path: &Path) -> Result<Coast, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
        let coast = Coast::from_geojson(&value);
        if coast.rings.is_empty() {
            return Err(format!("{} holds no polygons", path.display()));
        }
        Ok(coast)
    }

    /// Takes every Polygon and MultiPolygon in a FeatureCollection, a
    /// Feature or a bare geometry, and skips anything else.
    fn from_geojson(value: &Value) -> Coast {
        let mut rings = Vec::new();
        collect_rings(value, &mut rings);
        Coast { rings }
    }

    /// The stretches of each spoke that lie over land, as distances in
    /// meters from `boat` where the beam enters and leaves it, out to
    /// `max_range`. Spoke `i` points at true bearing `i / spokes` of a turn.
    pub(crate) fn spokes(
        &self,
        boat: &GeoPosition,
        spokes: usize,
        max_range: f64,
    ) -> Vec<Vec<(f64, f64)>> {
        let meters_per_degree_lon = METERS_PER_DEGREE_LAT * boat.lat().to_radians().cos();
        let to_local = |&(lon, lat): &(f64, f64)| {
            (
                // The short way round, so land across the antimeridian, as
                // off Fiji, lies beside the boat rather than a world away
                ((lon - boat.lon() + HALF_LONGITUDE_DEGREES).rem_euclid(FULL_LONGITUDE_DEGREES)
                    - HALF_LONGITUDE_DEGREES)
                    * meters_per_degree_lon,
                (lat - boat.lat()) * METERS_PER_DEGREE_LAT,
            )
        };

        // Each spoke only tests the edges within its angle, so a coast of
        // many thousand points stays cheap.
        let mut edges: Vec<((f64, f64), (f64, f64))> = Vec::new();
        let mut by_spoke: Vec<Vec<usize>> = vec![Vec::new(); spokes];
        // A boat on land, in a harbour the chart draws as land say, starts
        // every beam on land: one ray crossing an odd number of edges, out
        // to any distance, tells.
        let mut crossed_northwards = 0;
        for ring in &self.rings {
            let local: Vec<(f64, f64)> = ring.iter().map(to_local).collect();
            for pair in local.windows(2) {
                let (p, q) = (pair[0], pair[1]);
                if ray_crossing(0.0, 1.0, p, q).is_some() {
                    crossed_northwards += 1;
                }
                if distance_to_segment(p, q) > max_range {
                    continue;
                }
                let index = edges.len();
                edges.push((p, q));
                for spoke in spokes_between(p, q, spokes) {
                    by_spoke[spoke].push(index);
                }
            }
        }

        let on_land = crossed_northwards % 2 == 1;

        by_spoke
            .iter()
            .enumerate()
            .map(|(spoke, candidates)| {
                let bearing = spoke as f64 / spokes as f64 * TAU;
                let (sin_b, cos_b) = bearing.sin_cos();
                let mut crossings: Vec<f64> = candidates
                    .iter()
                    .filter_map(|&i| ray_crossing(sin_b, cos_b, edges[i].0, edges[i].1))
                    .filter(|&t| t < max_range)
                    .collect();
                crossings.sort_by(f64::total_cmp);
                if on_land {
                    crossings.insert(0, 0.0);
                }
                crossings
                    .chunks(2)
                    .map(|pair| (pair[0], pair.get(1).copied().unwrap_or(max_range)))
                    .collect()
            })
            .collect()
    }
}

fn collect_rings(value: &Value, rings: &mut Vec<Vec<(f64, f64)>>) {
    match value.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            for feature in value["features"].as_array().into_iter().flatten() {
                collect_rings(feature, rings);
            }
        }
        Some("Feature") => collect_rings(&value["geometry"], rings),
        Some("GeometryCollection") => {
            for geometry in value["geometries"].as_array().into_iter().flatten() {
                collect_rings(geometry, rings);
            }
        }
        Some("Polygon") => push_polygon(&value["coordinates"], rings),
        Some("MultiPolygon") => {
            for polygon in value["coordinates"].as_array().into_iter().flatten() {
                push_polygon(polygon, rings);
            }
        }
        _ => {}
    }
}

fn push_polygon(polygon: &Value, rings: &mut Vec<Vec<(f64, f64)>>) {
    for ring in polygon.as_array().into_iter().flatten() {
        let mut points: Vec<(f64, f64)> = ring
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|point| Some((point.get(0)?.as_f64()?, point.get(1)?.as_f64()?)))
            .collect();
        if points.len() < 3 {
            continue;
        }
        if points.first() != points.last() {
            points.push(points[0]);
        }
        rings.push(points);
    }
}

/// How close the segment from `p` to `q` comes to own ship at the origin.
fn distance_to_segment(p: (f64, f64), q: (f64, f64)) -> f64 {
    let (dx, dy) = (q.0 - p.0, q.1 - p.1);
    let length_sq = dx * dx + dy * dy;
    let t = if length_sq == 0.0 {
        0.0
    } else {
        (-(p.0 * dx + p.1 * dy) / length_sq).clamp(0.0, 1.0)
    };
    (p.0 + t * dx).hypot(p.1 + t * dy)
}

/// The spokes whose bearing lies within the angle the segment covers as
/// seen from the origin, ends included.
fn spokes_between(p: (f64, f64), q: (f64, f64), spokes: usize) -> impl Iterator<Item = usize> {
    let spoke_of = |(x, y): (f64, f64)| {
        let bearing = x.atan2(y).rem_euclid(TAU);
        (bearing / TAU * spokes as f64) as usize % spokes
    };
    let (a, b) = (spoke_of(p), spoke_of(q));
    // A straight edge seen from a point off it spans less than half a turn,
    // so the shorter way round is the right one.
    let forward = (b + spokes - a) % spokes;
    let (start, count) = if forward <= spokes / 2 {
        (a, forward)
    } else {
        (b, spokes - forward)
    };
    (0..=count).map(move |i| (start + i) % spokes)
}

/// Where a ray from the origin along bearing (`sin_b`, `cos_b`) crosses the
/// segment from `p` to `q`, as its distance along the ray. The segment
/// counts as crossed when its ends lie on different sides of the ray's line,
/// one of them strictly, so a ray through a vertex crosses once.
fn ray_crossing(sin_b: f64, cos_b: f64, p: (f64, f64), q: (f64, f64)) -> Option<f64> {
    let side = |(x, y): (f64, f64)| x * cos_b - y * sin_b;
    let (sp, sq) = (side(p), side(q));
    if (sp > 0.0) == (sq > 0.0) {
        return None;
    }
    let t = sp / (sp - sq);
    let (x, y) = (p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1));
    let along = x * sin_b + y * cos_b;
    (along > 0.0).then_some(along)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SPOKES: usize = 2048;

    /// A square of land whose near edge lies `near` meters north of the
    /// boat at (53.2, 5.3) and whose far edge lies `far` meters north,
    /// `half_width` meters to either side.
    fn square_north(near: f64, far: f64, half_width: f64) -> Value {
        let boat = GeoPosition::new(53.2, 5.3);
        let lat = |north: f64| boat.lat() + north / METERS_PER_DEGREE_LAT;
        let lon =
            |east: f64| boat.lon() + east / (METERS_PER_DEGREE_LAT * boat.lat().to_radians().cos());
        json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {},
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[
                        [lon(-half_width), lat(near)],
                        [lon(half_width), lat(near)],
                        [lon(half_width), lat(far)],
                        [lon(-half_width), lat(far)],
                        [lon(-half_width), lat(near)]
                    ]]
                }
            }]
        })
    }

    fn land(value: &Value, max_range: f64) -> Vec<Vec<(f64, f64)>> {
        Coast::from_geojson(value).spokes(&GeoPosition::new(53.2, 5.3), SPOKES, max_range)
    }

    /// From a boat inside the land every beam starts on land and leaves it
    /// at the shore, instead of the water and land swapping places.
    #[test]
    fn a_boat_on_land_sees_land_from_where_it_is() {
        let spokes = land(&square_north(-500.0, 500.0, 500.0), 3000.0);

        for spoke in [0, SPOKES / 4, SPOKES / 2, 3 * SPOKES / 4] {
            let beam = &spokes[spoke];
            assert_eq!(beam.len(), 1, "{beam:?}");
            assert_eq!(beam[0].0, 0.0, "{beam:?}");
            assert!((beam[0].1 - 500.0).abs() < 1.0, "{beam:?}");
        }
    }

    /// Land just across the antimeridian shows where it lies, east of a boat
    /// just west of it.
    #[test]
    fn land_across_the_antimeridian_lies_beside_the_boat() {
        let square = json!({
            "type": "Polygon",
            "coordinates": [[
                [-179.99, -17.01], [-179.98, -17.01], [-179.98, -16.99],
                [-179.99, -16.99], [-179.99, -17.01]
            ]]
        });
        let spokes =
            Coast::from_geojson(&square).spokes(&GeoPosition::new(-17.0, 179.99), SPOKES, 5000.0);

        let east = &spokes[SPOKES / 4];
        assert_eq!(east.len(), 1, "{east:?}");
        assert!(east[0].0 > 1000.0 && east[0].0 < 3000.0, "{east:?}");
        assert!(spokes[3 * SPOKES / 4].is_empty(), "nothing to the west");
    }

    /// The beam heading for the land enters it at its near edge and leaves
    /// at its far edge; a beam the other way sees none.
    #[test]
    fn a_spoke_towards_land_finds_where_it_enters_and_leaves() {
        let spokes = land(&square_north(1000.0, 1500.0, 300.0), 3000.0);

        let north = &spokes[0];
        assert_eq!(north.len(), 1);
        assert!((north[0].0 - 1000.0).abs() < 1.0, "{north:?}");
        assert!((north[0].1 - 1500.0).abs() < 1.0, "{north:?}");
        assert!(spokes[SPOKES / 2].is_empty(), "nothing to the south");
        assert!(spokes[SPOKES / 4].is_empty(), "nothing to the east");
    }

    /// Land that reaches past the range shows out to the range.
    #[test]
    fn land_beyond_the_range_is_cut_at_the_range() {
        let spokes = land(&square_north(1000.0, 5000.0, 300.0), 2000.0);

        assert_eq!(spokes[0], vec![(spokes[0][0].0, 2000.0)]);
        assert!(land(&square_north(3000.0, 5000.0, 300.0), 2000.0)[0].is_empty());
    }

    /// A lake in an island is water again.
    #[test]
    fn a_hole_in_the_land_is_water() {
        let mut island = square_north(1000.0, 2000.0, 500.0);
        let lake = square_north(1300.0, 1600.0, 100.0);
        let ring = lake["features"][0]["geometry"]["coordinates"][0].clone();
        island["features"][0]["geometry"]["coordinates"]
            .as_array_mut()
            .unwrap()
            .push(ring);

        let north = &land(&island, 3000.0)[0];

        assert_eq!(north.len(), 2, "{north:?}");
        assert!((north[0].1 - 1300.0).abs() < 1.0);
        assert!((north[1].0 - 1600.0).abs() < 1.0);
    }

    #[test]
    fn a_file_without_polygons_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("points.geojson");
        std::fs::write(
            &path,
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[5.3,53.2]}}"#,
        )
        .unwrap();

        assert!(Coast::load(&path).is_err());
    }

    /// Every GeoJSON root a chart export may use loads, other geometries in
    /// it are skipped, and a ring left open is closed.
    #[test]
    fn supported_geojson_roots_are_loaded_and_open_rings_are_closed() {
        let square = json!([
            [5.0, 53.0],
            [5.1, 53.0],
            [5.1, 53.1],
            [5.0, 53.1],
            [5.0, 53.0]
        ]);
        let open = json!([[6.0, 53.0], [6.1, 53.0], [6.1, 53.1]]);
        let cases = [
            (
                "polygon",
                json!({"type": "Polygon", "coordinates": [square]}),
                1,
            ),
            (
                "multipolygon",
                json!({"type": "MultiPolygon", "coordinates": [[square], [open]]}),
                2,
            ),
            (
                "collection",
                json!({"type": "GeometryCollection", "geometries": [
                    {"type": "Point", "coordinates": [5.0, 53.0]},
                    {"type": "Polygon", "coordinates": [open]}
                ]}),
                1,
            ),
            (
                "feature",
                json!({"type": "Feature", "properties": {}, "geometry":
                    {"type": "Polygon", "coordinates": [square, open]}}),
                2,
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        for (name, geojson, rings) in cases {
            let path = dir.path().join(format!("{name}.geojson"));
            std::fs::write(&path, geojson.to_string()).unwrap();

            let coast = Coast::load(&path).unwrap();
            assert_eq!(coast.rings.len(), rings, "{name}");
            for ring in &coast.rings {
                assert_eq!(ring.first(), ring.last(), "{name}: ring closed");
            }
        }
    }
}
