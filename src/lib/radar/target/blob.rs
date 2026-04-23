//! Blob detection for radar target tracking.
//!
//! This module detects contiguous groups of strong pixels (blobs) in radar spokes
//! and identifies those that meet ship size constraints. All blobs are sent to the
//! tracker which decides whether to track them based on:
//! - Guard zone presence (automatic acquisition)
//! - Existing tracked target proximity (continue tracking)
//! - MARPA (manual acquisition via user click)
//! - DopplerAutoTrack (automatic acquisition of Doppler-colored targets)

use std::collections::HashMap;
use std::f64::consts::TAU;

use crate::config::GuardZone;
use crate::protos::RadarMessage::radar_message::Spoke;

/// Default minimum pixel intensity to be considered part of a blob (2/3 of max 15, strong return).
/// This is overridden by legend.strong_return which varies per radar brand.
const DEFAULT_BLOB_THRESHOLD: u8 = 10;

/// Minimum number of strong-return pixels a blob must contain to be considered a valid target.
/// At 25km range each pixel is ~25m, so 25 pixels is the minimum for a plausible vessel return.
/// Thin streaks (wave crests, clutter arcs) typically have < 20 strong pixels despite large
/// bounding-box sizes; real vessels at this range produce dense clusters of 50+ pixels.
const MIN_TARGET_PIXELS: usize = 25;

/// Minimum ship size in meters
pub const MIN_TARGET_SIZE_M: f64 = 5.0;

/// Maximum ship size in meters
pub const MAX_TARGET_SIZE_M: f64 = 1000.0;

/// A single pixel belonging to a blob
#[derive(Clone, Debug)]
struct BlobPixel {
    spoke: u16,
    pixel: usize,
    #[allow(dead_code)] // May be useful for intensity-weighted center calculation
    intensity: u8,
}

/// A blob that is still being built as spokes arrive.
///
/// The radial extent (`min_pixel`..=`max_pixel`) is tracked incrementally
/// because pixel indices along a spoke are linear (0..sweep_len). The
/// angular extent is *not* tracked incrementally: spoke indices live on a
/// circle modulo `spokes_per_revolution`, so linear min/max would give the
/// wrong answer for blobs that straddle the 0/N-1 wrap-around point. The
/// spoke arc is instead computed from `pixels` on demand when the blob
/// completes; see `SpokeArc::from_blob`.
struct BlobInProgress {
    id: u32,
    pixels: Vec<BlobPixel>,
    last_spoke_with_addition: u16,
    min_pixel: usize,
    max_pixel: usize,
    /// True if any pixel in this blob has Doppler-approaching intensity
    has_doppler_approaching: bool,
}

impl BlobInProgress {
    fn new(id: u32, pixel: BlobPixel) -> Self {
        let pixel_idx = pixel.pixel;
        BlobInProgress {
            id,
            min_pixel: pixel_idx,
            max_pixel: pixel_idx,
            last_spoke_with_addition: pixel.spoke,
            has_doppler_approaching: false,
            pixels: vec![pixel],
        }
    }

    fn add_pixel(&mut self, pixel: BlobPixel, current_spoke: u16) {
        self.min_pixel = self.min_pixel.min(pixel.pixel);
        self.max_pixel = self.max_pixel.max(pixel.pixel);
        self.last_spoke_with_addition = current_spoke;
        self.pixels.push(pixel);
    }

    /// Absorb another blob's pixels and bounds. The detector-level index is
    /// updated separately by the caller.
    fn absorb(&mut self, other: BlobInProgress, current_spoke: u16) {
        self.min_pixel = self.min_pixel.min(other.min_pixel);
        self.max_pixel = self.max_pixel.max(other.max_pixel);
        self.last_spoke_with_addition = current_spoke;
        self.has_doppler_approaching |= other.has_doppler_approaching;
        self.pixels.extend(other.pixels);
    }
}

/// The smallest circular arc on the spoke domain that covers every spoke a
/// blob touches, together with its length and center. Computed once per blob
/// at completion time, not maintained incrementally.
#[derive(Debug, Clone, Copy)]
struct SpokeArc {
    /// Length of the arc in spokes (1..=spokes_per_revolution).
    extent: u16,
    /// Center spoke of the arc (the spoke at `floor(extent / 2)` positions
    /// forward from the arc's starting spoke, modulo spokes_per_revolution).
    center: u16,
}

impl SpokeArc {
    /// Compute the smallest covering arc for the distinct spokes a blob
    /// touches.
    ///
    /// Each pair of adjacent spokes on the circle delimits a "gap" — a run
    /// of consecutive empty spoke positions between them. There are exactly
    /// as many gaps as there are distinct spokes (one gap between each
    /// adjacent pair going around the full circle). The sum of all gap
    /// lengths equals `spokes_per_revolution - distinct_spoke_count`.
    ///
    /// The smallest arc covering all the blob's spokes is the *complement*
    /// of the largest such gap: remove the largest run of empty positions
    /// from the circle and what's left must contain every occupied spoke.
    ///
    /// This is correct for both non-wrapping blobs (where the largest gap
    /// is the wrap-around gap via spoke 0 and the arc is the linear
    /// [min..=max] range) and wrap-around blobs (where the largest gap sits
    /// in the middle of the uncovered region and the arc straddles spoke 0).
    fn from_blob(blob: &BlobInProgress, spokes_per_revolution: u16) -> SpokeArc {
        debug_assert!(!blob.pixels.is_empty(), "blob must have at least one pixel");
        debug_assert!(spokes_per_revolution > 0);

        let mut spokes: Vec<u16> = blob.pixels.iter().map(|p| p.spoke).collect();
        spokes.sort_unstable();
        spokes.dedup();

        if spokes.len() == 1 {
            return SpokeArc {
                extent: 1,
                center: spokes[0],
            };
        }

        // Largest run of consecutive empty spokes between occupied ones.
        // The gap between sorted adjacent spokes `a` and `b` (a < b) holds
        // `b - a - 1` empty positions. The wrap gap from the last spoke
        // forward past spoke 0 to the first spoke holds
        // `spokes_per_revolution - last + first - 1` empty positions.
        let mut largest_gap: u16 = 0;
        // Index in `spokes` of the arc's starting spoke (the one immediately
        // after the largest empty gap going forward around the circle).
        // Defaults to 0 meaning "the arc starts at spokes[0]", which is
        // correct when the largest gap is the wrap-around gap.
        let mut arc_start_idx: usize = 0;

        for i in 0..spokes.len() - 1 {
            let gap = spokes[i + 1] - spokes[i] - 1;
            if gap > largest_gap {
                largest_gap = gap;
                arc_start_idx = i + 1;
            }
        }

        let wrap_gap =
            spokes_per_revolution - spokes[spokes.len() - 1] + spokes[0] - 1;
        if wrap_gap > largest_gap {
            largest_gap = wrap_gap;
            arc_start_idx = 0;
        }

        let extent = spokes_per_revolution - largest_gap;
        let arc_start = spokes[arc_start_idx];
        let center = ((arc_start as u32 + (extent as u32 / 2))
            % spokes_per_revolution as u32) as u16;

        SpokeArc { extent, center }
    }
}

/// A completed blob with contour information
#[derive(Clone)]
pub struct CompletedBlob {
    pub contour: Vec<(u16, usize)>,
    /// All pixels in the blob (for debug visualization)
    pub all_pixels: Vec<(u16, usize)>,
    pub center_spoke: u16,
    pub center_pixel: usize,
    pub size_meters: f64,
    /// Which guard zones contain this blob's center (1 and/or 2), empty if none
    pub in_guard_zones: Vec<u8>,
    /// True if any pixel in this blob has Doppler-approaching intensity
    pub has_doppler_approaching: bool,
}

/// Internal representation of a guard zone in spoke/pixel coordinates
#[derive(Clone, Debug)]
struct GuardZoneInternal {
    /// Guard zone number (1 or 2)
    zone_id: u8,
    /// Start angle in spokes
    start_spoke: u16,
    /// End angle in spokes
    end_spoke: u16,
    /// Inner distance in pixels
    start_pixel: usize,
    /// Outer distance in pixels
    end_pixel: usize,
}

// Guard zones allow negative head-relative angles, so conversion to the
// circular spoke domain must wrap around 0 instead of collapsing negatives.
fn radians_to_spoke(angle_radians: f64, spokes_per_revolution: u16) -> u16 {
    let spoke = ((angle_radians / TAU) * spokes_per_revolution as f64) as i32;
    spoke.rem_euclid(spokes_per_revolution as i32) as u16
}

/// Blob detector that processes spokes and identifies targets
pub struct BlobDetector {
    spokes_per_revolution: u16,
    /// Minimum pixel intensity to be considered part of a blob (from legend.strong_return)
    threshold: u8,
    /// Pixel intensity range for Doppler-approaching returns: `(first, last)`
    /// inclusive. From `legend.doppler_approaching` `(start, count)`.
    doppler_approaching_range: Option<(u8, u8)>,
    next_blob_id: u32,
    /// Active blobs keyed by stable blob id so merges/removals don't invalidate references.
    active_blobs: HashMap<u32, BlobInProgress>,
    /// Detector-wide spatial index: (spoke, pixel) -> id of the blob that owns that pixel.
    /// Enables O(1) adjacency lookup independent of the number of active blobs.
    pixel_index: HashMap<(u16, usize), u32>,
    current_range: u32,
    current_spoke_len: usize,
    /// Cached guard zone configs for refresh on range change
    guard_zone_1: Option<GuardZone>,
    guard_zone_2: Option<GuardZone>,
    /// Active guard zones in spoke/pixel coordinates
    guard_zones: Vec<GuardZoneInternal>,
}

impl BlobDetector {
    pub fn new(
        spokes_per_revolution: u16,
        threshold: u8,
        doppler_approaching: Option<(u8, u8)>,
    ) -> Self {
        let threshold = if threshold > 0 {
            threshold
        } else {
            DEFAULT_BLOB_THRESHOLD
        };
        // Convert (start, count) to (start, end_inclusive) for O(1) range checks.
        let doppler_approaching_range =
            doppler_approaching.map(|(start, count)| (start, start + count - 1));
        BlobDetector {
            spokes_per_revolution,
            threshold,
            doppler_approaching_range,
            next_blob_id: 0,
            active_blobs: HashMap::new(),
            pixel_index: HashMap::new(),
            current_range: 0,
            current_spoke_len: 0,
            guard_zone_1: None,
            guard_zone_2: None,
            guard_zones: Vec::new(),
        }
    }

    /// Set guard zone 1 config (call when control changes)
    pub fn set_guard_zone_1(&mut self, zone: Option<GuardZone>) {
        self.guard_zone_1 = zone;
        self.refresh_guard_zones();
    }

    /// Set guard zone 2 config (call when control changes)
    pub fn set_guard_zone_2(&mut self, zone: Option<GuardZone>) {
        self.guard_zone_2 = zone;
        self.refresh_guard_zones();
    }

    /// Refresh guard zones from cached config (call when range/spoke_len changes)
    fn refresh_guard_zones(&mut self) {
        if self.current_range == 0 || self.current_spoke_len == 0 {
            if !self.guard_zones.is_empty() {
                self.guard_zones.clear();
            }
            return;
        }

        let meters_per_pixel = self.current_range as f64 / self.current_spoke_len as f64;

        // Build new guard zones
        let mut new_zones = Vec::new();
        for (zone_id, zone_opt) in [(1u8, &self.guard_zone_1), (2u8, &self.guard_zone_2)] {
            if let Some(zone) = zone_opt {
                if !zone.enabled {
                    continue;
                }

                // Guard zones are head-relative (0 = forward) and can cross
                // 0 with a negative start angle.
                let start_spoke =
                    radians_to_spoke(zone.start_angle, self.spokes_per_revolution);
                let end_spoke =
                    radians_to_spoke(zone.end_angle, self.spokes_per_revolution);

                // Convert distances from meters to pixels
                let start_pixel = (zone.start_distance / meters_per_pixel) as usize;
                let end_pixel = (zone.end_distance / meters_per_pixel) as usize;

                new_zones.push(GuardZoneInternal {
                    zone_id,
                    start_spoke,
                    end_spoke,
                    start_pixel,
                    end_pixel,
                });
            }
        }

        // Only update and log if zones changed
        let changed = new_zones.len() != self.guard_zones.len()
            || new_zones
                .iter()
                .zip(self.guard_zones.iter())
                .any(|(new, old)| {
                    new.zone_id != old.zone_id
                        || new.start_spoke != old.start_spoke
                        || new.end_spoke != old.end_spoke
                        || new.start_pixel != old.start_pixel
                        || new.end_pixel != old.end_pixel
                });

        if changed {
            for gz in &new_zones {
                log::debug!(
                    "Guard zone {}: spokes {}-{}, pixels {}-{}",
                    gz.zone_id,
                    gz.start_spoke,
                    gz.end_spoke,
                    gz.start_pixel,
                    gz.end_pixel
                );
            }
            self.guard_zones = new_zones;
        }
    }

    /// Check which guard zones contain a given spoke/pixel position
    fn check_guard_zones(&self, spoke: u16, pixel: usize) -> Vec<u8> {
        let mut zones = Vec::new();

        for gz in &self.guard_zones {
            // Check pixel (distance) is within range
            if pixel < gz.start_pixel || pixel > gz.end_pixel {
                continue;
            }

            // Check spoke (angle) is within range, handling wraparound
            let in_angle = if gz.start_spoke <= gz.end_spoke {
                // Normal case: start < end
                spoke >= gz.start_spoke && spoke <= gz.end_spoke
            } else {
                // Wraparound case: zone spans 0
                spoke >= gz.start_spoke || spoke <= gz.end_spoke
            };

            if in_angle {
                zones.push(gz.zone_id);
            }
        }

        zones
    }

    /// Calculate the physical size of a blob in meters
    fn calculate_size(&self, blob: &BlobInProgress, spoke_arc: &SpokeArc) -> f64 {
        if self.current_range == 0 || self.current_spoke_len == 0 {
            return 0.0;
        }

        let meters_per_pixel = self.current_range as f64 / self.current_spoke_len as f64;

        // Radial extent
        let radial_extent = (blob.max_pixel - blob.min_pixel + 1) as f64 * meters_per_pixel;

        // Angular extent (at average distance). The spoke arc is the
        // smallest circular range of spokes the blob touches — handles
        // wrap-around correctly unlike linear min/max.
        let avg_distance = (blob.min_pixel + blob.max_pixel) as f64 / 2.0 * meters_per_pixel;
        let angular_extent = avg_distance
            * (spoke_arc.extent as f64 * TAU / self.spokes_per_revolution as f64);

        // Use larger dimension as "size"
        radial_extent.max(angular_extent)
    }

    /// Calculate the contour (edge pixels) of a blob.
    /// A pixel is on the contour if any of its 4 neighbors is not part of the
    /// same blob in the detector-level spatial index.
    fn calculate_contour(&self, blob: &BlobInProgress) -> Vec<(u16, usize)> {
        blob.pixels
            .iter()
            .filter(|p| {
                let prev_spoke = if p.spoke == 0 {
                    self.spokes_per_revolution - 1
                } else {
                    p.spoke - 1
                };
                let next_spoke = (p.spoke + 1) % self.spokes_per_revolution;

                let neighbors = [
                    (p.spoke, p.pixel.wrapping_sub(1)), // inner
                    (p.spoke, p.pixel + 1),             // outer
                    (prev_spoke, p.pixel),              // ccw
                    (next_spoke, p.pixel),              // cw
                ];

                neighbors
                    .iter()
                    .any(|key| self.pixel_index.get(key).copied() != Some(blob.id))
            })
            .map(|p| (p.spoke, p.pixel))
            .collect()
    }

    /// Return the set of blob ids whose pixels are 8-neighbors of (spoke, pixel_idx).
    fn adjacent_blob_ids(&self, spoke: u16, pixel_idx: usize) -> Vec<u32> {
        let prev_spoke = if spoke == 0 {
            self.spokes_per_revolution - 1
        } else {
            spoke - 1
        };
        let next_spoke = (spoke + 1) % self.spokes_per_revolution;

        let mut ids: Vec<u32> = Vec::new();
        for &s in &[prev_spoke, spoke, next_spoke] {
            for dp in [-1i64, 0, 1] {
                let p = pixel_idx as i64 + dp;
                if p < 0 {
                    continue;
                }
                if let Some(&id) = self.pixel_index.get(&(s, p as usize)) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        ids
    }

    /// Process a single spoke and return any completed blobs
    pub fn process_spoke(&mut self, spoke: &Spoke) -> Vec<CompletedBlob> {
        // Update range and spoke length if changed, then refresh guard zones
        let spoke_len = spoke.data.len();
        let range_changed = spoke.range != 0 && spoke.range != self.current_range;
        let spoke_len_changed = spoke_len != 0 && spoke_len != self.current_spoke_len;

        if range_changed {
            self.current_range = spoke.range;
            log::debug!("BlobDetector: range updated to {}m", self.current_range);
        }
        if spoke_len_changed {
            self.current_spoke_len = spoke_len;
        }
        if range_changed || spoke_len_changed {
            self.refresh_guard_zones();
        }

        // Use spoke.angle (head-relative) for guard zone checks since guard zones
        // are defined relative to boat heading, not true north
        let spoke_angle = spoke.angle as u16 % self.spokes_per_revolution;

        // Find strong pixels (strong return) and Doppler-approaching pixels.
        // Doppler pixels have a distinct intensity value outside the normal return scale
        // so they are collected alongside strong pixels regardless of threshold.
        let mut strong_pixels: Vec<BlobPixel> = Vec::new();
        for (pixel_idx, &intensity) in spoke.data.iter().enumerate() {
            let is_doppler_approaching = self
                .doppler_approaching_range
                .map(|(lo, hi)| intensity >= lo && intensity <= hi)
                .unwrap_or(false);
            if intensity >= self.threshold || is_doppler_approaching {
                strong_pixels.push(BlobPixel {
                    spoke: spoke_angle,
                    pixel: pixel_idx,
                    intensity,
                });
            }
        }

        // Process each strong pixel using the detector-level spatial index.
        for pixel in strong_pixels {
            let is_doppler_approaching = self
                .doppler_approaching_range
                .map(|(lo, hi)| pixel.intensity >= lo && pixel.intensity <= hi)
                .unwrap_or(false);

            let adjacent_ids = self.adjacent_blob_ids(pixel.spoke, pixel.pixel);

            let target_id = match adjacent_ids.len() {
                0 => {
                    let id = self.next_blob_id;
                    self.next_blob_id += 1;
                    let mut blob = BlobInProgress::new(id, pixel.clone());
                    blob.has_doppler_approaching = is_doppler_approaching;
                    self.active_blobs.insert(id, blob);
                    self.pixel_index.insert((pixel.spoke, pixel.pixel), id);
                    continue;
                }
                1 => adjacent_ids[0],
                _ => {
                    // Merge all adjacent blobs into the one with the lowest id
                    // (stable across iterations). Reassign their pixels in the index.
                    let survivor = *adjacent_ids.iter().min().unwrap();
                    for id in adjacent_ids.iter().copied().filter(|id| *id != survivor) {
                        let absorbed = self
                            .active_blobs
                            .remove(&id)
                            .expect("absorbed blob must exist");
                        for p in &absorbed.pixels {
                            self.pixel_index.insert((p.spoke, p.pixel), survivor);
                        }
                        self.active_blobs
                            .get_mut(&survivor)
                            .expect("survivor blob must exist")
                            .absorb(absorbed, spoke_angle);
                    }
                    survivor
                }
            };

            let blob = self
                .active_blobs
                .get_mut(&target_id)
                .expect("target blob must exist");
            blob.has_doppler_approaching |= is_doppler_approaching;
            let spoke = pixel.spoke;
            let pixel_idx = pixel.pixel;
            blob.add_pixel(pixel, spoke_angle);
            self.pixel_index.insert((spoke, pixel_idx), target_id);
        }

        // Check for completed blobs (not extended on this spoke nor the previous one)
        let prev_spoke = if spoke_angle == 0 {
            self.spokes_per_revolution - 1
        } else {
            spoke_angle - 1
        };
        let completed_ids: Vec<u32> = self
            .active_blobs
            .iter()
            .filter_map(|(&id, blob)| {
                if blob.last_spoke_with_addition != spoke_angle
                    && blob.last_spoke_with_addition != prev_spoke
                {
                    Some(id)
                } else {
                    None
                }
            })
            .collect();

        let mut completed: Vec<CompletedBlob> = Vec::new();
        for id in completed_ids {
            let blob = self
                .active_blobs
                .remove(&id)
                .expect("completed blob must exist");
            let spoke_arc = SpokeArc::from_blob(&blob, self.spokes_per_revolution);
            let size = self.calculate_size(&blob, &spoke_arc);
            let pixel_count = blob.pixels.len();
            let valid = pixel_count >= MIN_TARGET_PIXELS
                && size >= MIN_TARGET_SIZE_M
                && size <= MAX_TARGET_SIZE_M;
            log::debug!(
                "BlobDetector: completed blob with {} pixels, size {:.1}m (valid: {})",
                pixel_count,
                size,
                valid
            );
            if valid {
                let contour = self.calculate_contour(&blob);
                let all_pixels: Vec<(u16, usize)> =
                    blob.pixels.iter().map(|p| (p.spoke, p.pixel)).collect();
                let center_spoke = spoke_arc.center;
                let center_pixel = (blob.min_pixel + blob.max_pixel) / 2;
                let in_guard_zones = self.check_guard_zones(center_spoke, center_pixel);
                completed.push(CompletedBlob {
                    contour,
                    all_pixels,
                    center_spoke,
                    center_pixel,
                    size_meters: size,
                    in_guard_zones,
                    has_doppler_approaching: blob.has_doppler_approaching,
                });
            }
            // Drop this blob's entries from the detector-level spatial index.
            for p in &blob.pixels {
                self.pixel_index.remove(&(p.spoke, p.pixel));
            }
        }

        completed
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn blob_from_spokes(spokes: &[u16]) -> BlobInProgress {
        let mut blob = BlobInProgress::new(
            0,
            BlobPixel {
                spoke: spokes[0],
                pixel: 0,
                intensity: 15,
            },
        );
        for &s in &spokes[1..] {
            blob.add_pixel(
                BlobPixel {
                    spoke: s,
                    pixel: 0,
                    intensity: 15,
                },
                s,
            );
        }
        blob
    }

    #[test]
    fn spoke_arc_single_spoke() {
        let blob = blob_from_spokes(&[42]);
        let arc = SpokeArc::from_blob(&blob, 1024);
        assert_eq!(arc.extent, 1);
        assert_eq!(arc.center, 42);
    }

    #[test]
    fn spoke_arc_contiguous_no_wrap() {
        let blob = blob_from_spokes(&[100, 101, 102, 103, 104, 105, 106, 107, 108, 109]);
        let arc = SpokeArc::from_blob(&blob, 1024);
        assert_eq!(arc.extent, 10);
        assert_eq!(arc.center, 105);
    }

    #[test]
    fn spoke_arc_wraps_across_zero() {
        // Blob spans spokes 1018..=1023, 0, 1, 2 in a 1024-spoke revolution.
        // Smallest covering arc is 9 spokes long, centered ~1022.
        let blob = blob_from_spokes(&[1018, 1019, 1020, 1021, 1022, 1023, 0, 1, 2]);
        let arc = SpokeArc::from_blob(&blob, 1024);
        assert_eq!(arc.extent, 9);
        assert_eq!(arc.center, 1022);
    }

    #[test]
    fn spoke_arc_touches_zero_without_wrap() {
        // Blob ends exactly at spoke 1023 coming from the high side
        // (spokes 1020..=1023, no spoke 0). This is still a non-wrapping
        // blob: the arc is [1020, 1023].
        let blob = blob_from_spokes(&[1020, 1021, 1022, 1023]);
        let arc = SpokeArc::from_blob(&blob, 1024);
        assert_eq!(arc.extent, 4);
        assert_eq!(arc.center, 1022);
    }

    #[test]
    fn spoke_arc_starts_at_zero() {
        // Blob starts at spoke 0 going up. Non-wrapping: arc is [0, 3].
        let blob = blob_from_spokes(&[0, 1, 2, 3]);
        let arc = SpokeArc::from_blob(&blob, 1024);
        assert_eq!(arc.extent, 4);
        assert_eq!(arc.center, 2);
    }

    #[test]
    fn spoke_arc_ignores_duplicate_pixels_on_same_spoke() {
        // Two pixels on the same spoke must not inflate the arc.
        let mut blob = blob_from_spokes(&[10, 11, 12]);
        // Add another pixel on spoke 11 (different radial position).
        blob.add_pixel(
            BlobPixel {
                spoke: 11,
                pixel: 5,
                intensity: 15,
            },
            11,
        );
        let arc = SpokeArc::from_blob(&blob, 1024);
        assert_eq!(arc.extent, 3);
        assert_eq!(arc.center, 11);
    }

    #[test]
    fn spoke_arc_scattered_wraparound_blob() {
        // Blob with spokes 8180, 8185, 8190, 0, 5 in an 8192-spoke
        // revolution. Empty gaps on the circle:
        //   8180 -> 8185:   4 empty
        //   8185 -> 8190:   4 empty
        //   8190 -> 0:      1 empty (spoke 8191)
        //   0    -> 5:      4 empty
        //   5    -> 8180:   8174 empty  <- largest, from 5 forward to 8180
        // So the smallest covering arc starts at 8180 and has length
        // 8192 - 8174 = 18, wrapping through 0 to 5. Center sits 9 spokes
        // forward of 8180, which is spoke 8189.
        let blob = blob_from_spokes(&[8180, 8185, 8190, 0, 5]);
        let arc = SpokeArc::from_blob(&blob, 8192);
        assert_eq!(arc.extent, 18);
        assert_eq!(arc.center, 8189);
    }

    #[test]
    fn spoke_arc_two_adjacent_spokes_at_wrap() {
        // Exactly the spokes 8191 and 0 in an 8192-spoke revolution. Arc
        // must be 2 spokes long, not 8192.
        let blob = blob_from_spokes(&[8191, 0]);
        let arc = SpokeArc::from_blob(&blob, 8192);
        assert_eq!(arc.extent, 2);
        // Arc starts at 8191 (the spoke after the largest empty gap from
        // 0 forward to 8191, which is 8190 empty slots). Center =
        // (8191 + 1) % 8192 = 0.
        assert_eq!(arc.center, 0);
    }

    #[test]
    fn guard_zone_negative_angles_wrap_correctly() {
        // A sector like -55 deg .. 38 deg must wrap across spoke 0.
        let mut detector = BlobDetector::new(2048, 10, None);

        // The actual range/pixel values only need to be non-zero so the guard
        // zone cache can be rebuilt.
        detector.current_range = 1000;
        detector.current_spoke_len = 1000;
        detector.set_guard_zone_1(Some(GuardZone {
            start_angle: -55.0 * PI / 180.0,
            end_angle: 38.0 * PI / 180.0,
            start_distance: 0.0,
            end_distance: 1000.0,
            enabled: true,
        }));

        assert_eq!(detector.guard_zones.len(), 1);

        let zone = &detector.guard_zones[0];

        // The start must land near the end of the revolution, proving we
        // wrapped the negative angle instead of collapsing it to 0.
        assert!(zone.start_spoke > zone.end_spoke);
        assert!(zone.start_spoke > 1700);
        assert!(zone.end_spoke < 300);

        // A spoke in the negative-angle part of the sector must be detected.
        assert_eq!(detector.check_guard_zones(1800, 500), vec![1]);

        // A spoke in the positive-angle part must also be detected.
        assert_eq!(detector.check_guard_zones(150, 500), vec![1]);

        // A spoke well outside the configured sector must not match.
        assert!(detector.check_guard_zones(900, 500).is_empty());
    }
}
