//! Navigation marks from a GeoJSON file, so buoys and beacons a chart shows
//! echo on the emulator's picture.

use std::path::Path;

use serde_json::Value;

use crate::radar::GeoPosition;

/// A mark the radar sees: a buoy with its steel or radar reflector returns
/// well, a beacon on a stake or a withy hardly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mark {
    pub position: GeoPosition,
    pub strong: bool,
}

/// The marks of a file, and how many of its points could not be placed
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Marks {
    pub marks: Vec<Mark>,
    /// Points whose coordinates are not two numbers
    pub malformed: usize,
    /// Points outside longitude and latitude, as a projected export gives
    pub out_of_range: usize,
}

impl Marks {
    /// What was left out and why, if anything was
    pub(crate) fn left_out(&self) -> Option<String> {
        let reasons: Vec<String> = [
            (self.malformed, "coordinates that are not numbers"),
            (
                self.out_of_range,
                "coordinates outside longitude and latitude, as in a projected export",
            ),
        ]
        .into_iter()
        .filter(|&(count, _)| count > 0)
        .map(|(count, why)| {
            let points = if count == 1 { "point" } else { "points" };
            format!("{count} {points} with {why}")
        })
        .collect();
        (!reasons.is_empty()).then(|| reasons.join(" and "))
    }
}

/// Takes every Point and MultiPoint in a FeatureCollection, a Feature or a
/// bare geometry, and skips anything else. A feature whose `echo` property
/// is `"weak"` returns weakly; every other mark returns strongly. A file
/// none of whose points can be placed is refused, saying why.
pub(crate) fn load(path: &Path) -> Result<Marks, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
    let found = from_geojson(&value);
    if !found.marks.is_empty() {
        return Ok(found);
    }
    Err(match found.left_out() {
        Some(left_out) => format!("{} holds no usable points: {left_out}", path.display()),
        None => format!("{} holds no points", path.display()),
    })
}

fn from_geojson(value: &Value) -> Marks {
    let mut found = Marks::default();
    collect_marks(value, true, &mut found);
    found
}

fn collect_marks(value: &Value, strong: bool, found: &mut Marks) {
    match value.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            for feature in value["features"].as_array().into_iter().flatten() {
                collect_marks(feature, strong, found);
            }
        }
        Some("Feature") => {
            let weak = value["properties"]["echo"].as_str() == Some("weak");
            collect_marks(&value["geometry"], !weak, found);
        }
        Some("GeometryCollection") => {
            for geometry in value["geometries"].as_array().into_iter().flatten() {
                collect_marks(geometry, strong, found);
            }
        }
        Some("Point") => push_point(&value["coordinates"], strong, found),
        Some("MultiPoint") => {
            for point in value["coordinates"].as_array().into_iter().flatten() {
                push_point(point, strong, found);
            }
        }
        _ => {}
    }
}

fn push_point(point: &Value, strong: bool, found: &mut Marks) {
    match (point[0].as_f64(), point[1].as_f64()) {
        (Some(lon), Some(lat))
            if (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat) =>
        {
            found.marks.push(Mark {
                position: GeoPosition::new(lat, lon),
                strong,
            });
        }
        (Some(_), Some(_)) => found.out_of_range += 1,
        _ => found.malformed += 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn points_become_marks_and_other_geometry_is_skipped() {
        let found = from_geojson(&json!({
            "type": "FeatureCollection",
            "features": [
                {"type": "Feature", "properties": {"OBJL": 17},
                 "geometry": {"type": "Point", "coordinates": [5.3, 53.2]}},
                {"type": "Feature", "properties": {"echo": "weak"},
                 "geometry": {"type": "MultiPoint", "coordinates": [[5.31, 53.21], [5.32, 53.22]]}},
                {"type": "Feature", "properties": null,
                 "geometry": {"type": "LineString", "coordinates": [[5.0, 53.0], [5.1, 53.1]]}},
                {"type": "Feature", "properties": {},
                 "geometry": {"type": "Point", "coordinates": ["5.3", 53.2]}},
                {"type": "Feature", "properties": {},
                 "geometry": {"type": "Point", "coordinates": [500000.0, 5700000.0]}}
            ]
        }));

        assert_eq!(found.malformed, 1, "a point in text is left out");
        assert_eq!(found.out_of_range, 1, "a projected point is left out");
        assert_eq!(
            found.marks,
            vec![
                Mark {
                    position: GeoPosition::new(53.2, 5.3),
                    strong: true
                },
                Mark {
                    position: GeoPosition::new(53.21, 5.31),
                    strong: false
                },
                Mark {
                    position: GeoPosition::new(53.22, 5.32),
                    strong: false
                },
            ]
        );
    }

    #[test]
    fn a_file_without_points_is_refused() {
        let path =
            std::env::temp_dir().join(format!("mayara-marks-{}.geojson", std::process::id()));
        std::fs::write(
            &path,
            r#"{"type":"Polygon","coordinates":[[[5,53],[5.1,53],[5.1,53.1],[5,53]]]}"#,
        )
        .unwrap();
        let loaded = load(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(loaded.unwrap_err().contains("holds no points"));
    }

    fn load_text(name: &str, text: &str) -> Result<Marks, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        load(&path)
    }

    /// A file mixing usable and projected points loads the usable ones and
    /// counts the rest, so the emulator can warn of them.
    #[test]
    fn a_projected_point_beside_usable_ones_is_counted() {
        let found = load_text(
            "mixed.geojson",
            r#"{"type":"MultiPoint","coordinates":[[5.3,53.2],[500000,5700000]]}"#,
        )
        .unwrap();
        assert_eq!(found.marks.len(), 1);
        assert_eq!(found.out_of_range, 1);
        assert_eq!(found.malformed, 0);
    }

    #[test]
    fn a_file_in_projected_coordinates_is_refused_saying_why() {
        let refused = load_text(
            "utm.geojson",
            r#"{"type":"MultiPoint","coordinates":[[500000,5700000],[500100,5700100]]}"#,
        )
        .unwrap_err();
        assert!(
            refused.contains("2 points with coordinates outside longitude and latitude"),
            "{refused}"
        );
        assert!(!refused.contains("not numbers"), "{refused}");
    }

    #[test]
    fn a_file_with_malformed_coordinates_is_refused_saying_why() {
        let refused = load_text(
            "text.geojson",
            r#"{"type":"MultiPoint","coordinates":[["5.3","53.2"],[5.3],[500000,5700000]]}"#,
        )
        .unwrap_err();
        assert!(
            refused.contains("2 points with coordinates that are not numbers")
                && refused.contains("1 point with coordinates outside"),
            "{refused}"
        );
    }
}
