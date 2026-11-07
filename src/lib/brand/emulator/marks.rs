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

/// Takes every Point and MultiPoint in a FeatureCollection, a Feature or a
/// bare geometry, and skips anything else. A feature whose `echo` property
/// is `"weak"` returns weakly; every other mark returns strongly. Points
/// outside longitude and latitude, as a projected export would give, are
/// left out with a warning.
pub(crate) fn load(path: &Path) -> Result<Vec<Mark>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
    let (marks, off_globe) = from_geojson(&value);
    if off_globe > 0 {
        log::warn!(
            "{}: left out {off_globe} points not in longitude and latitude",
            path.display()
        );
    }
    if marks.is_empty() {
        return Err(format!(
            "{} holds no points in longitude and latitude",
            path.display()
        ));
    }
    Ok(marks)
}

/// The marks, and how many points lay outside longitude and latitude
fn from_geojson(value: &Value) -> (Vec<Mark>, usize) {
    let mut marks = Vec::new();
    collect_marks(value, true, &mut marks);
    let before = marks.len();
    marks.retain(|mark| {
        (-90.0..=90.0).contains(&mark.position.lat())
            && (-180.0..=180.0).contains(&mark.position.lon())
    });
    let off_globe = before - marks.len();
    (marks, off_globe)
}

fn collect_marks(value: &Value, strong: bool, marks: &mut Vec<Mark>) {
    match value.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            for feature in value["features"].as_array().into_iter().flatten() {
                collect_marks(feature, strong, marks);
            }
        }
        Some("Feature") => {
            let weak = value["properties"]["echo"].as_str() == Some("weak");
            collect_marks(&value["geometry"], !weak, marks);
        }
        Some("GeometryCollection") => {
            for geometry in value["geometries"].as_array().into_iter().flatten() {
                collect_marks(geometry, strong, marks);
            }
        }
        Some("Point") => push_point(&value["coordinates"], strong, marks),
        Some("MultiPoint") => {
            for point in value["coordinates"].as_array().into_iter().flatten() {
                push_point(point, strong, marks);
            }
        }
        _ => {}
    }
}

fn push_point(point: &Value, strong: bool, marks: &mut Vec<Mark>) {
    if let (Some(lon), Some(lat)) = (point[0].as_f64(), point[1].as_f64()) {
        marks.push(Mark {
            position: GeoPosition::new(lat, lon),
            strong,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn points_become_marks_and_other_geometry_is_skipped() {
        let (marks, off_globe) = from_geojson(&json!({
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

        assert_eq!(off_globe, 1, "a projected point is left out");
        assert_eq!(
            marks,
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

    #[test]
    fn a_file_in_projected_coordinates_is_refused() {
        let path =
            std::env::temp_dir().join(format!("mayara-marks-utm-{}.geojson", std::process::id()));
        std::fs::write(&path, r#"{"type":"Point","coordinates":[500000,5700000]}"#).unwrap();
        let loaded = load(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(loaded.unwrap_err().contains("in longitude and latitude"));
    }
}
