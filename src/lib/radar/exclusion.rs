//! Exclusion zone mask for stationary radar installations.
//!
//! Exclusion zones are areas where radar returns are suppressed (set to transparent).
//! The mask is precomputed when the range changes to ensure O(1) lookup during spoke processing.

use std::f64::consts::TAU;

use crate::config::{ExclusionRect, ExclusionZone};

/// Internal representation of a sector exclusion zone in spoke/pixel coordinates
#[derive(Debug, Clone)]
pub struct ExclusionZoneInternal {
    pub start_spoke: u16,
    pub end_spoke: u16,
    pub start_pixel: usize,
    pub end_pixel: usize,
}

/// Internal representation of a rectangular exclusion zone.
/// Stores corner-based coordinates for rotated rectangles.
#[derive(Debug, Clone)]
pub struct ExclusionRectInternal {
    pub x1: f64,    // First corner X (meters from radar)
    pub y1: f64,    // First corner Y (meters from radar)
    pub x2: f64,    // Second corner X (defines one edge)
    pub y2: f64,    // Second corner Y (defines one edge)
    pub width: f64, // Perpendicular width
}

/// Precomputed exclusion mask for efficient spoke processing.
///
/// Uses a bitmask to store exclusion state per pixel, allowing O(1) lookup.
/// The mask is rebuilt when range changes or exclusion zones are modified.
pub struct ExclusionMask {
    /// Bitmask: 1 bit per pixel (0=not excluded, 1=excluded)
    /// Indexed by [spoke][pixel / 8], bit = pixel % 8
    mask: Vec<Vec<u8>>,
    spokes: u16,
    pixels_per_spoke: usize,
}

/// Check if a point is inside a convex polygon using cross product signs
fn is_point_in_polygon(px: f64, py: f64, corners: &[(f64, f64)]) -> bool {
    let n = corners.len();
    if n < 3 {
        return false;
    }

    // Check if point is on the same side of all edges
    let mut sign = None;
    for i in 0..n {
        let (ax, ay) = corners[i];
        let (bx, by) = corners[(i + 1) % n];

        // Vector from a to b
        let edge_x = bx - ax;
        let edge_y = by - ay;

        // Vector from a to point
        let to_point_x = px - ax;
        let to_point_y = py - ay;

        // Cross product: positive if point is left of edge, negative if right
        let cross = edge_x * to_point_y - edge_y * to_point_x;

        if cross.abs() < 1e-10 {
            continue; // Point is on the edge, consider it inside
        }

        let current_sign = cross > 0.0;
        match sign {
            None => sign = Some(current_sign),
            Some(s) if s != current_sign => return false,
            _ => {}
        }
    }

    true
}

impl ExclusionMask {
    /// Create a new exclusion mask from zone configurations.
    ///
    /// # Arguments
    /// * `zones` - Active sector exclusion zones in internal coordinates
    /// * `rects` - Active rectangular exclusion zones in meters
    /// * `spokes` - Number of spokes per revolution
    /// * `pixels` - Number of pixels per spoke
    /// * `range_meters` - Current radar range in meters
    pub fn new(
        zones: &[ExclusionZoneInternal],
        rects: &[ExclusionRectInternal],
        spokes: u16,
        pixels: usize,
        range_meters: u32,
    ) -> Self {
        // Each byte stores 8 pixels (1 bit each)
        let bytes_per_spoke = (pixels + 7) / 8;
        let mut mask = vec![vec![0u8; bytes_per_spoke]; spokes as usize];

        // Build the mask for all sector zones
        for zone in zones {
            for spoke in 0..spokes {
                // Check spoke (angle) is within range, handling wraparound
                let in_angle = if zone.start_spoke <= zone.end_spoke {
                    spoke >= zone.start_spoke && spoke <= zone.end_spoke
                } else {
                    // Wraparound case: zone spans 0
                    spoke >= zone.start_spoke || spoke <= zone.end_spoke
                };

                if !in_angle {
                    continue;
                }

                // Set bits for all pixels in the distance range
                let start = zone.start_pixel.min(pixels);
                let end = zone.end_pixel.min(pixels);
                for pixel in start..=end {
                    let byte_idx = pixel / 8;
                    let bit = pixel % 8;
                    mask[spoke as usize][byte_idx] |= 1 << bit;
                }
            }
        }

        // Build the mask for all rectangular zones
        // Rectangle is defined by corners (x1,y1), (x2,y2) and width
        // We compute the 4 corners and test ray intersection with the quadrilateral
        let meters_per_pixel = range_meters as f64 / pixels as f64;

        for rect in rects {
            // Compute the 4 corners of the rectangle
            // Edge from (x1,y1) to (x2,y2), then extend perpendicular by width
            let dx = rect.x2 - rect.x1;
            let dy = rect.y2 - rect.y1;
            let edge_len = (dx * dx + dy * dy).sqrt();
            if edge_len < 1e-6 || rect.width < 1e-6 {
                continue; // Degenerate rectangle
            }

            // Perpendicular unit vector (rotated 90 degrees clockwise)
            let perp_x = dy / edge_len;
            let perp_y = -dx / edge_len;

            // 4 corners of the rectangle
            let corners = [
                (rect.x1, rect.y1),
                (rect.x2, rect.y2),
                (rect.x2 + perp_x * rect.width, rect.y2 + perp_y * rect.width),
                (rect.x1 + perp_x * rect.width, rect.y1 + perp_y * rect.width),
            ];

            // Check if origin (0,0) is inside the rectangle using winding number
            let origin_inside = is_point_in_polygon(0.0, 0.0, &corners);

            for spoke in 0..spokes {
                // Ray direction for this spoke (0 = north, increasing clockwise)
                let angle = (spoke as f64 / spokes as f64) * TAU;
                let ray_dx = angle.sin(); // x component (east)
                let ray_dy = angle.cos(); // y component (north)

                // Find ray intersection with the quadrilateral (4 edges)
                let mut intersections: Vec<f64> = Vec::new();

                for i in 0..4 {
                    let (ax, ay) = corners[i];
                    let (bx, by) = corners[(i + 1) % 4];

                    // Edge vector
                    let ex = bx - ax;
                    let ey = by - ay;

                    // Solve: origin + t * ray = a + s * edge
                    let denom = ray_dx * ey - ray_dy * ex;
                    if denom.abs() < 1e-10 {
                        continue; // Ray parallel to edge
                    }

                    // t = distance along ray, s = parameter along edge [0,1]
                    let t = (ax * ey - ay * ex) / denom;
                    let s = (ax * ray_dy - ay * ray_dx) / denom;

                    // Check if intersection is within edge bounds and ray is positive
                    if t > 0.0 && (0.0..=1.0).contains(&s) {
                        intersections.push(t);
                    }
                }

                // Sort intersections
                intersections.sort_by(|a, b| a.partial_cmp(b).unwrap());

                // Determine range to exclude
                let (d_min, d_max) = if origin_inside {
                    // Origin inside: exclude from 0 to first exit
                    if !intersections.is_empty() {
                        (0.0, intersections[0])
                    } else {
                        continue;
                    }
                } else {
                    // Origin outside: need entry and exit
                    if intersections.len() >= 2 {
                        (intersections[0], intersections[1])
                    } else {
                        continue;
                    }
                };

                // Convert distances to pixel indices
                let start_pixel = (d_min / meters_per_pixel) as usize;
                let end_pixel = ((d_max / meters_per_pixel) as usize).min(pixels.saturating_sub(1));

                // Set bits for all pixels in range
                for pixel in start_pixel..=end_pixel {
                    let byte_idx = pixel / 8;
                    let bit = pixel % 8;
                    mask[spoke as usize][byte_idx] |= 1 << bit;
                }
            }
        }

        ExclusionMask {
            mask,
            spokes,
            pixels_per_spoke: pixels,
        }
    }

    /// Check if a pixel is excluded.
    #[inline]
    pub fn is_excluded(&self, spoke: u16, pixel: usize) -> bool {
        if spoke >= self.spokes || pixel >= self.pixels_per_spoke {
            return false;
        }

        let byte_idx = pixel / 8;
        let bit = pixel % 8;
        (self.mask[spoke as usize][byte_idx] >> bit) & 1 == 1
    }
}

/// Convert exclusion zone from config to internal spoke/pixel coordinates.
pub fn zone_to_internal(
    zone: &ExclusionZone,
    spokes_per_revolution: u16,
    range_meters: u32,
    spoke_len: usize,
) -> ExclusionZoneInternal {
    let meters_per_pixel = range_meters as f64 / spoke_len as f64;

    // Convert angles from radians to spokes
    let start_spoke = ((zone.start_angle / TAU) * spokes_per_revolution as f64) as i32;
    let start_spoke = start_spoke.rem_euclid(spokes_per_revolution as i32) as u16;

    let end_spoke = ((zone.end_angle / TAU) * spokes_per_revolution as f64) as i32;
    let end_spoke = end_spoke.rem_euclid(spokes_per_revolution as i32) as u16;

    // Convert distances from meters to pixels
    let start_pixel = (zone.start_distance / meters_per_pixel) as usize;
    let end_pixel = (zone.end_distance / meters_per_pixel).min(spoke_len as f64) as usize;

    ExclusionZoneInternal {
        start_spoke,
        end_spoke,
        start_pixel,
        end_pixel,
    }
}

/// Convert rectangular exclusion zone from config to internal format.
pub fn rect_to_internal(rect: &ExclusionRect) -> ExclusionRectInternal {
    ExclusionRectInternal {
        x1: rect.x1,
        y1: rect.y1,
        x2: rect.x2,
        y2: rect.y2,
        width: rect.width,
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    #[test]
    fn test_exclusion_mask_empty() {
        let mask = ExclusionMask::new(&[], &[], 360, 512, 1000);
        assert!(!mask.is_excluded(0, 0));
        assert!(!mask.is_excluded(180, 256));
    }

    #[test]
    fn test_exclusion_mask_simple_zone() {
        let zones = vec![ExclusionZoneInternal {
            start_spoke: 10,
            end_spoke: 20,
            start_pixel: 100,
            end_pixel: 200,
        }];
        let mask = ExclusionMask::new(&zones, &[], 360, 512, 1000);

        // Outside zone
        assert!(!mask.is_excluded(5, 150));
        assert!(!mask.is_excluded(15, 50));
        assert!(!mask.is_excluded(15, 250));

        // Inside zone
        assert!(mask.is_excluded(10, 100));
        assert!(mask.is_excluded(15, 150));
        assert!(mask.is_excluded(20, 200));

        // Boundary checks
        assert!(mask.is_excluded(10, 150)); // start spoke
        assert!(mask.is_excluded(20, 150)); // end spoke
        assert!(mask.is_excluded(15, 100)); // start pixel
        assert!(mask.is_excluded(15, 200)); // end pixel
    }

    #[test]
    fn test_exclusion_mask_wraparound() {
        // Zone that wraps around 0
        let zones = vec![ExclusionZoneInternal {
            start_spoke: 350,
            end_spoke: 10,
            start_pixel: 100,
            end_pixel: 200,
        }];
        let mask = ExclusionMask::new(&zones, &[], 360, 512, 1000);

        // Inside zone (before wrap)
        assert!(mask.is_excluded(350, 150));
        assert!(mask.is_excluded(355, 150));
        assert!(mask.is_excluded(359, 150));
        // Inside zone (after wrap)
        assert!(mask.is_excluded(0, 150));
        assert!(mask.is_excluded(5, 150));
        assert!(mask.is_excluded(10, 150));
        // Outside zone
        assert!(!mask.is_excluded(180, 150));
        assert!(!mask.is_excluded(11, 150));
        assert!(!mask.is_excluded(349, 150));
    }

    #[test]
    fn test_zone_to_internal() {
        let zone = ExclusionZone {
            start_angle: 0.0,
            end_angle: PI / 2.0, // 90 degrees
            start_distance: 100.0,
            end_distance: 500.0,
            enabled: true,
        };

        let internal = zone_to_internal(&zone, 360, 1000, 1000);

        assert_eq!(internal.start_spoke, 0);
        assert_eq!(internal.end_spoke, 90);
        assert_eq!(internal.start_pixel, 100);
        assert_eq!(internal.end_pixel, 500);
    }

    #[test]
    fn test_exclusion_mask_rectangle() {
        // Rectangle: edge from (-100, 100) to (100, 100) with width 200
        // This creates a rectangle from y=100 down to y=-100 (width extends in -Y direction)
        // Corners: (-100, 100), (100, 100), (100, -100), (-100, -100)
        // With 1000m range and 1000 pixels, each pixel is 1m
        let rects = vec![ExclusionRectInternal {
            x1: -100.0,
            y1: 100.0,
            x2: 100.0,
            y2: 100.0,
            width: 200.0,
        }];
        let mask = ExclusionMask::new(&[], &rects, 360, 1000, 1000);

        // Center (pixel 0) should be excluded
        assert!(mask.is_excluded(0, 0));
        assert!(mask.is_excluded(90, 0));
        assert!(mask.is_excluded(180, 0));
        assert!(mask.is_excluded(270, 0));

        // At 50m north (spoke 0, pixel ~50) should be excluded
        assert!(mask.is_excluded(0, 50));

        // At 150m north (spoke 0, pixel ~150) should NOT be excluded
        assert!(!mask.is_excluded(0, 150));

        // At 50m east (spoke 90, pixel ~50) should be excluded
        assert!(mask.is_excluded(90, 50));

        // At 50m south (spoke 180, pixel ~50) should be excluded
        assert!(mask.is_excluded(180, 50));

        // At 50m west (spoke 270, pixel ~50) should be excluded
        assert!(mask.is_excluded(270, 50));
    }

    #[test]
    fn test_rect_to_internal() {
        let rect = ExclusionRect {
            x1: 0.0,
            y1: 100.0,
            x2: 200.0,
            y2: 100.0,
            width: 50.0,
            enabled: true,
        };

        let internal = rect_to_internal(&rect);

        assert_eq!(internal.x1, 0.0);
        assert_eq!(internal.y1, 100.0);
        assert_eq!(internal.x2, 200.0);
        assert_eq!(internal.y2, 100.0);
        assert_eq!(internal.width, 50.0);
    }
}
