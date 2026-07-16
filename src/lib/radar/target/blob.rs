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

/// Sentinel in `pixel_index` meaning "no blob owns this pixel". Blob ids
/// start at 1 so 0 is always available as the "unowned" marker.
const UNOWNED: u32 = 0;

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

/// Hard cap on the pixels retained by one in-progress blob. A blob that is
/// extended on every spoke (e.g. a saturated clutter disk around own ship)
/// never satisfies the completion check, so without a cap it would hold pixels
/// for as long as the return persists. No plausible vessel return comes
/// anywhere near this count (a valid target has at most a few thousand
/// pixels); oversized blobs are discarded outright.
const MAX_BLOB_PIXELS: usize = 100_000;

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
    fn from_blob(
        blob: &BlobInProgress,
        spokes_per_revolution: u16,
        scratch: &mut Vec<u16>,
    ) -> SpokeArc {
        debug_assert!(!blob.pixels.is_empty(), "blob must have at least one pixel");
        debug_assert!(spokes_per_revolution > 0);

        scratch.clear();
        scratch.extend(blob.pixels.iter().map(|p| p.spoke));
        scratch.sort_unstable();
        scratch.dedup();
        let spokes: &[u16] = scratch;

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

        let wrap_gap = spokes_per_revolution - spokes[spokes.len() - 1] + spokes[0] - 1;
        if wrap_gap > largest_gap {
            largest_gap = wrap_gap;
            arc_start_idx = 0;
        }

        let extent = spokes_per_revolution - largest_gap;
        let arc_start = spokes[arc_start_idx];
        let center =
            ((arc_start as u32 + (extent as u32 / 2)) % spokes_per_revolution as u32) as u16;

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
    /// Detector-wide spatial index sized `spokes_per_revolution * current_spoke_len`,
    /// indexed as `spoke * current_spoke_len + pixel`. Each cell stores the id of
    /// the blob that owns that pixel, or `UNOWNED` (0) if none.
    ///
    /// A flat `Vec<u32>` is right for this dense, bounded coordinate space:
    /// lookups and inserts are pointer arithmetic instead of tuple hashing, and
    /// the peak footprint is bounded by the radar's spoke geometry (a few tens
    /// of MiB at most) instead of scaling with the number of live pixels.
    pixel_index: Vec<u32>,
    /// Scratch buffers reused across `process_spoke` calls to keep the hot
    /// path allocation-free after warm-up. Each is taken out via `mem::take`
    /// at the start of the call, mutated freely alongside `&mut self`, and
    /// put back at the end so its capacity survives.
    adjacent_ids_scratch: Vec<u32>,
    completed_ids_scratch: Vec<u32>,
    spoke_arc_scratch: Vec<u16>,
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
            next_blob_id: 1,
            active_blobs: HashMap::new(),
            pixel_index: Vec::new(),
            adjacent_ids_scratch: Vec::new(),
            completed_ids_scratch: Vec::new(),
            spoke_arc_scratch: Vec::new(),
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
                let start_spoke = radians_to_spoke(zone.start_angle, self.spokes_per_revolution);
                let end_spoke = radians_to_spoke(zone.end_angle, self.spokes_per_revolution);

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
        let angular_extent =
            avg_distance * (spoke_arc.extent as f64 * TAU / self.spokes_per_revolution as f64);

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
                    .any(|&(s, p)| self.pixel_owner(s, p) != blob.id)
            })
            .map(|p| (p.spoke, p.pixel))
            .collect()
    }

    /// Fill `out` with the distinct blob ids whose pixels are 8-neighbors of
    /// (spoke, pixel_idx). Cleared on entry so callers can hand in a scratch
    /// buffer with retained capacity.
    fn adjacent_blob_ids_into(&self, spoke: u16, pixel_idx: usize, out: &mut Vec<u32>) {
        out.clear();
        let prev_spoke = if spoke == 0 {
            self.spokes_per_revolution - 1
        } else {
            spoke - 1
        };
        let next_spoke = (spoke + 1) % self.spokes_per_revolution;

        for &s in &[prev_spoke, spoke, next_spoke] {
            for dp in [-1i64, 0, 1] {
                let p = pixel_idx as i64 + dp;
                if p < 0 {
                    continue;
                }
                let id = self.pixel_owner(s, p as usize);
                if id != UNOWNED && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }

    /// Flat-array index for `(spoke, pixel)`. Callers guarantee both fit
    /// within `spokes_per_revolution` and `current_spoke_len`.
    fn idx(&self, spoke: u16, pixel: usize) -> usize {
        spoke as usize * self.current_spoke_len + pixel
    }

    /// Blob id owning `(spoke, pixel)`, or `UNOWNED` if none. Returns
    /// `UNOWNED` for out-of-range pixels so neighbour walks don't need to
    /// bounds-check the pixel axis separately.
    fn pixel_owner(&self, spoke: u16, pixel: usize) -> u32 {
        if pixel >= self.current_spoke_len {
            return UNOWNED;
        }
        self.pixel_index[self.idx(spoke, pixel)]
    }

    fn set_pixel_owner(&mut self, spoke: u16, pixel: usize, id: u32) {
        let i = self.idx(spoke, pixel);
        self.pixel_index[i] = id;
    }

    fn clear_pixel(&mut self, spoke: u16, pixel: usize) {
        let i = self.idx(spoke, pixel);
        self.pixel_index[i] = UNOWNED;
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
            // Pixel coordinates now index a different physical distance, so
            // any in-progress blob and its spatial-index entries no longer
            // describe the same object. Discard both and reallocate the
            // spatial index to the new dimensions.
            self.current_spoke_len = spoke_len;
            self.active_blobs.clear();
            self.pixel_index = vec![UNOWNED; self.spokes_per_revolution as usize * spoke_len];
        }
        if range_changed || spoke_len_changed {
            self.refresh_guard_zones();
        }

        // Use spoke.angle (head-relative) for guard zone checks since guard zones
        // are defined relative to boat heading, not true north
        let spoke_angle = spoke.angle as u16 % self.spokes_per_revolution;

        // Take scratch buffers out of self so they can be mutated freely
        // alongside `&mut self` calls in the pixel and completion loops.
        // Put back at the end so their capacity is preserved for next spoke.
        let mut adjacent_ids = std::mem::take(&mut self.adjacent_ids_scratch);
        let mut completed_ids = std::mem::take(&mut self.completed_ids_scratch);
        let mut spoke_arc_scratch = std::mem::take(&mut self.spoke_arc_scratch);

        // Fused strong-pixel detection and processing: a single pass over
        // spoke.data replaces the previous "build Vec<BlobPixel> then iterate"
        // pattern, saving a per-spoke allocation.
        let doppler_range = self.doppler_approaching_range;
        for (pixel_idx, &intensity) in spoke.data.iter().enumerate() {
            let is_doppler_approaching = doppler_range
                .map(|(lo, hi)| intensity >= lo && intensity <= hi)
                .unwrap_or(false);
            if intensity < self.threshold && !is_doppler_approaching {
                continue;
            }
            let pixel = BlobPixel {
                spoke: spoke_angle,
                pixel: pixel_idx,
                intensity,
            };

            self.adjacent_blob_ids_into(pixel.spoke, pixel.pixel, &mut adjacent_ids);

            let target_id = match adjacent_ids.len() {
                0 => {
                    let id = self.next_blob_id;
                    self.next_blob_id += 1;
                    let mut blob = BlobInProgress::new(id, pixel.clone());
                    blob.has_doppler_approaching = is_doppler_approaching;
                    self.active_blobs.insert(id, blob);
                    self.set_pixel_owner(pixel.spoke, pixel.pixel, id);
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
                            self.set_pixel_owner(p.spoke, p.pixel, survivor);
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
            let (pxl_spoke, pxl_pixel) = (pixel.spoke, pixel.pixel);
            let cell = pxl_spoke as usize * self.current_spoke_len + pxl_pixel;
            if self.pixel_index[cell] == target_id {
                // The pixel already belongs to this blob from an earlier
                // revolution — possible only for a blob that never completes,
                // e.g. a clutter ring touching every bearing. Re-pushing it
                // would grow `pixels` without bound; just refresh liveness.
                blob.last_spoke_with_addition = spoke_angle;
            } else {
                blob.add_pixel(pixel, spoke_angle);
                self.pixel_index[cell] = target_id;
            }
            // Checked on both paths: a merge can push the blob over the cap
            // even when the current pixel is a duplicate.
            if blob.pixels.len() > MAX_BLOB_PIXELS {
                let blob = self
                    .active_blobs
                    .remove(&target_id)
                    .expect("oversized blob must exist");
                for p in &blob.pixels {
                    self.clear_pixel(p.spoke, p.pixel);
                }
                log::debug!(
                    "BlobDetector: discarded oversized blob with {} pixels",
                    blob.pixels.len()
                );
            }
        }

        // Check for completed blobs (not extended on this spoke nor the previous one)
        let prev_spoke = if spoke_angle == 0 {
            self.spokes_per_revolution - 1
        } else {
            spoke_angle - 1
        };
        completed_ids.clear();
        completed_ids.extend(self.active_blobs.iter().filter_map(|(&id, blob)| {
            if blob.last_spoke_with_addition != spoke_angle
                && blob.last_spoke_with_addition != prev_spoke
            {
                Some(id)
            } else {
                None
            }
        }));

        let mut completed: Vec<CompletedBlob> = Vec::new();
        for &id in completed_ids.iter() {
            let blob = self
                .active_blobs
                .remove(&id)
                .expect("completed blob must exist");
            let spoke_arc =
                SpokeArc::from_blob(&blob, self.spokes_per_revolution, &mut spoke_arc_scratch);
            let size = self.calculate_size(&blob, &spoke_arc);
            let pixel_count = blob.pixels.len();
            let valid = pixel_count >= MIN_TARGET_PIXELS
                && (MIN_TARGET_SIZE_M..=MAX_TARGET_SIZE_M).contains(&size);
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
                self.clear_pixel(p.spoke, p.pixel);
            }
        }

        // Put scratch buffers back so their capacity survives to the next call.
        self.adjacent_ids_scratch = adjacent_ids;
        self.completed_ids_scratch = completed_ids;
        self.spoke_arc_scratch = spoke_arc_scratch;

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
        let arc = SpokeArc::from_blob(&blob, 1024, &mut Vec::new());
        assert_eq!(arc.extent, 1);
        assert_eq!(arc.center, 42);
    }

    #[test]
    fn spoke_arc_contiguous_no_wrap() {
        let blob = blob_from_spokes(&[100, 101, 102, 103, 104, 105, 106, 107, 108, 109]);
        let arc = SpokeArc::from_blob(&blob, 1024, &mut Vec::new());
        assert_eq!(arc.extent, 10);
        assert_eq!(arc.center, 105);
    }

    #[test]
    fn spoke_arc_wraps_across_zero() {
        // Blob spans spokes 1018..=1023, 0, 1, 2 in a 1024-spoke revolution.
        // Smallest covering arc is 9 spokes long, centered ~1022.
        let blob = blob_from_spokes(&[1018, 1019, 1020, 1021, 1022, 1023, 0, 1, 2]);
        let arc = SpokeArc::from_blob(&blob, 1024, &mut Vec::new());
        assert_eq!(arc.extent, 9);
        assert_eq!(arc.center, 1022);
    }

    #[test]
    fn spoke_arc_touches_zero_without_wrap() {
        // Blob ends exactly at spoke 1023 coming from the high side
        // (spokes 1020..=1023, no spoke 0). This is still a non-wrapping
        // blob: the arc is [1020, 1023].
        let blob = blob_from_spokes(&[1020, 1021, 1022, 1023]);
        let arc = SpokeArc::from_blob(&blob, 1024, &mut Vec::new());
        assert_eq!(arc.extent, 4);
        assert_eq!(arc.center, 1022);
    }

    #[test]
    fn spoke_arc_starts_at_zero() {
        // Blob starts at spoke 0 going up. Non-wrapping: arc is [0, 3].
        let blob = blob_from_spokes(&[0, 1, 2, 3]);
        let arc = SpokeArc::from_blob(&blob, 1024, &mut Vec::new());
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
        let arc = SpokeArc::from_blob(&blob, 1024, &mut Vec::new());
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
        let arc = SpokeArc::from_blob(&blob, 8192, &mut Vec::new());
        assert_eq!(arc.extent, 18);
        assert_eq!(arc.center, 8189);
    }

    #[test]
    fn spoke_arc_two_adjacent_spokes_at_wrap() {
        // Exactly the spokes 8191 and 0 in an 8192-spoke revolution. Arc
        // must be 2 spokes long, not 8192.
        let blob = blob_from_spokes(&[8191, 0]);
        let arc = SpokeArc::from_blob(&blob, 8192, &mut Vec::new());
        assert_eq!(arc.extent, 2);
        // Arc starts at 8191 (the spoke after the largest empty gap from
        // 0 forward to 8191, which is 8190 empty slots). Center =
        // (8191 + 1) % 8192 = 0.
        assert_eq!(arc.center, 0);
    }

    /// Maximum return intensity for a 4-bit pixel, well above the test
    /// detector's threshold of 10.
    const STRONG_RETURN: u8 = 15;
    /// Range in meters for synthetic test spokes.
    const TEST_RANGE_M: u32 = 1000;

    /// Number of pixel_index cells claimed by any blob.
    fn occupied_cells(detector: &BlobDetector) -> usize {
        detector
            .pixel_index
            .iter()
            .filter(|&&id| id != UNOWNED)
            .count()
    }

    fn spoke_with(angle: u16, len: usize, strong: &[usize]) -> Spoke {
        let mut data = vec![0u8; len];
        for &p in strong {
            data[p] = STRONG_RETURN;
        }
        let mut spoke = Spoke::new();
        spoke.angle = angle as u32;
        spoke.range = TEST_RANGE_M;
        spoke.data = data;
        spoke
    }

    #[test]
    fn ring_blob_does_not_accumulate_duplicate_pixels() {
        // A return present at every bearing (near-range clutter ring) is
        // extended on every spoke, so it never satisfies the completion
        // check. Its retained pixels must stay bounded by its geometric
        // extent instead of growing every revolution (issue #434).
        const SPOKES: u16 = 64;
        let mut detector = BlobDetector::new(SPOKES, 10, None);

        let mut completed = Vec::new();
        for _rev in 0..4 {
            for angle in 0..SPOKES {
                completed.extend(detector.process_spoke(&spoke_with(angle, 512, &[5])));
            }
        }

        assert!(completed.is_empty());
        assert_eq!(detector.active_blobs.len(), 1);
        let ring = detector.active_blobs.values().next().unwrap();
        assert_eq!(ring.pixels.len(), SPOKES as usize);
        assert_eq!(occupied_cells(&detector), SPOKES as usize);
    }

    #[test]
    fn discrete_blob_still_completes_alongside_ring() {
        // A normal discrete target must keep completing once per revolution
        // even while an immortal ring blob is active.
        const SPOKES: u16 = 64;
        let mut detector = BlobDetector::new(SPOKES, 10, None);

        let mut completions = 0;
        for _rev in 0..3 {
            for angle in 0..SPOKES {
                let strong: Vec<usize> = if (10..13).contains(&angle) {
                    std::iter::once(5).chain(100..111).collect()
                } else {
                    vec![5]
                };
                completions += detector
                    .process_spoke(&spoke_with(angle, 512, &strong))
                    .len();
            }
        }

        assert_eq!(completions, 3);
        assert_eq!(occupied_cells(&detector), SPOKES as usize);
    }

    #[test]
    fn oversized_blob_is_discarded() {
        // A fully saturated disk merges into one blob that never completes;
        // the pixel cap must keep the detector's retained state bounded.
        const SPOKES: u16 = 64;
        let mut detector = BlobDetector::new(SPOKES, 10, None);
        let all: Vec<usize> = (0..2048).collect();

        let mut completed = Vec::new();
        for angle in 0..SPOKES {
            completed.extend(detector.process_spoke(&spoke_with(angle, 2048, &all)));
        }

        assert!(completed.is_empty());
        assert!(
            detector
                .active_blobs
                .values()
                .all(|b| b.pixels.len() <= MAX_BLOB_PIXELS)
        );
        assert!(occupied_cells(&detector) <= MAX_BLOB_PIXELS + 1);
    }

    #[test]
    fn spoke_len_change_resets_spatial_state() {
        // A range change that alters spoke length reshuffles what each
        // (spoke, pixel) coordinate refers to physically. Any in-progress
        // blob must be discarded so a subsequent adjacency check can't
        // mis-associate a pixel with a blob from the previous range.
        const SPOKES: u16 = 64;
        let mut detector = BlobDetector::new(SPOKES, 10, None);

        // Seed a persistent ring at spoke_len 512 so a blob is live in
        // active_blobs and pixel_index.
        for angle in 0..SPOKES {
            let _ = detector.process_spoke(&spoke_with(angle, 512, &[5]));
        }
        assert!(!detector.active_blobs.is_empty());
        assert!(occupied_cells(&detector) > 0);

        // First spoke at a new spoke_len must trigger the reset.
        let _ = detector.process_spoke(&spoke_with(0, 1024, &[]));

        assert!(detector.active_blobs.is_empty());
        assert_eq!(occupied_cells(&detector), 0);
        assert_eq!(
            detector.pixel_index.len(),
            SPOKES as usize * 1024,
            "spatial index must be sized for the new spoke length"
        );
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
