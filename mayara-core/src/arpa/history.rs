//! History Buffer for ARPA Target Tracking
//!
//! Maintains a circular buffer of radar spoke data for target detection and tracking.

use bitflags::bitflags;

use super::contour::{Contour, ContourError, MAX_CONTOUR_LENGTH, MIN_CONTOUR_LENGTH};
use super::doppler::DopplerState;
use super::polar::{Polar, FOUR_DIRECTIONS};

bitflags! {
    /// Pixel flags in history buffer for ARPA tracking
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct HistoryPixel: u8 {
        /// Pixel is above threshold (target detected)
        const TARGET = 0b1000_0000;
        /// Backup bit - pixel was target in previous scan
        const BACKUP = 0b0100_0000;
        /// Doppler approaching target
        const APPROACHING = 0b0010_0000;
        /// Doppler receding target
        const RECEDING = 0b0001_0000;
        /// Part of a target contour (for visualization)
        const CONTOUR = 0b0000_1000;

        /// Initial state for new pixel: TARGET | BACKUP
        const INITIAL = Self::TARGET.bits() | Self::BACKUP.bits();
        /// Mask to clear target bits
        const NO_TARGET = !(Self::TARGET.bits() | Self::BACKUP.bits());
    }
}

impl Default for HistoryPixel {
    fn default() -> Self {
        HistoryPixel::empty()
    }
}

impl HistoryPixel {
    /// Create a new history pixel with initial state
    pub fn new() -> Self {
        HistoryPixel::empty()
    }

    /// Check if pixel matches the given Doppler state
    pub fn matches_doppler(&self, doppler: &DopplerState) -> bool {
        let is_target = self.contains(HistoryPixel::TARGET);
        let is_backup = self.contains(HistoryPixel::BACKUP);
        let is_approaching = self.contains(HistoryPixel::APPROACHING);
        let is_receding = self.contains(HistoryPixel::RECEDING);

        doppler.matches_pixel(is_target, is_backup, is_approaching, is_receding)
    }
}

/// A single spoke's history data
#[derive(Debug, Clone)]
pub struct HistorySpoke {
    /// Pixel flags for each sample in the spoke
    pub sweep: Vec<HistoryPixel>,
    /// Timestamp when this spoke was received (ms)
    pub time: u64,
    /// Own ship latitude at time of reception (degrees)
    pub lat: f64,
    /// Own ship longitude at time of reception (degrees)
    pub lon: f64,
}

impl HistorySpoke {
    pub fn new(time: u64, lat: f64, lon: f64) -> Self {
        Self {
            sweep: Vec::new(),
            time,
            lat,
            lon,
        }
    }

    pub fn with_capacity(capacity: usize, time: u64, lat: f64, lon: f64) -> Self {
        Self {
            sweep: vec![HistoryPixel::new(); capacity],
            time,
            lat,
            lon,
        }
    }
}

/// Legend values for pixel classification
#[derive(Debug, Clone, Copy)]
pub struct Legend {
    /// Minimum value to consider as strong return (target)
    pub strong_return: u8,
    /// Value indicating Doppler approaching
    pub doppler_approaching: u8,
    /// Value indicating Doppler receding
    pub doppler_receding: u8,
    /// Value to draw contour border
    pub border: u8,
}

impl Default for Legend {
    fn default() -> Self {
        Self {
            strong_return: 64,
            doppler_approaching: 255,
            doppler_receding: 254,
            border: 253,
        }
    }
}

/// Pure history buffer - no I/O, just algorithms
///
/// Maintains spoke history and provides contour detection algorithms.
#[derive(Debug, Clone)]
pub struct HistoryBuffer {
    /// Spoke history data
    pub spokes: Vec<HistorySpoke>,
    /// Number of spokes per revolution
    spokes_per_revolution: usize,
}

impl HistoryBuffer {
    /// Create a new history buffer
    pub fn new(spokes_per_revolution: usize) -> Self {
        let spokes = (0..spokes_per_revolution)
            .map(|_| HistorySpoke::new(0, 0.0, 0.0))
            .collect();

        Self {
            spokes,
            spokes_per_revolution,
        }
    }

    /// Reset the buffer (e.g., on range change)
    pub fn reset(&mut self) {
        for spoke in &mut self.spokes {
            spoke.sweep.clear();
            spoke.time = 0;
        }
    }

    /// Normalize angle to [0, spokes_per_revolution)
    #[inline]
    pub fn mod_spokes(&self, angle: i32) -> usize {
        ((angle % self.spokes_per_revolution as i32) + self.spokes_per_revolution as i32) as usize
            % self.spokes_per_revolution
    }

    /// Get spoke length
    pub fn spoke_len(&self) -> usize {
        self.spokes.first().map(|s| s.sweep.len()).unwrap_or(0)
    }

    /// Check if a pixel matches the given Doppler state
    pub fn pix(&self, doppler: &DopplerState, angle: i32, r: i32) -> bool {
        let r = r as usize;
        if r >= self.spoke_len() || r < 3 {
            return false;
        }
        let angle_idx = self.mod_spokes(angle);
        if angle_idx >= self.spokes.len() {
            return false;
        }

        self.spokes[angle_idx]
            .sweep
            .get(r)
            .map(|p| p.matches_doppler(doppler))
            .unwrap_or(false)
    }

    /// Check if a blob has at least MIN_CONTOUR_LENGTH pixels
    /// Returns true if yes, false otherwise (and clears the blob if too small)
    pub fn multi_pix(&mut self, doppler: &DopplerState, angle: i32, r: i32) -> bool {
        if !self.pix(doppler, angle, r) {
            return false;
        }

        let length = MIN_CONTOUR_LENGTH;
        let start = Polar::new(angle, r, 0);
        let mut current = start;

        let mut max_angle = current;
        let mut min_angle = current;
        let mut max_r = current;
        let mut min_r = current;
        let mut count = 0;
        let mut found = false;

        // Find the orientation of border point
        let mut index = 0;
        for i in 0..4 {
            if !self.pix(
                doppler,
                current.angle + FOUR_DIRECTIONS[i].angle,
                current.r + FOUR_DIRECTIONS[i].r,
            ) {
                found = true;
                index = i;
                break;
            }
        }
        if !found {
            return false; // Single pixel or internal point
        }

        index = (index + 1) % 4;
        found = false;

        // Follow contour
        while current.r != start.r || current.angle != start.angle || count == 0 {
            index = (index + 3) % 4; // Turn left

            for _ in 0..4 {
                if self.pix(
                    doppler,
                    current.angle + FOUR_DIRECTIONS[index].angle,
                    current.r + FOUR_DIRECTIONS[index].r,
                ) {
                    found = true;
                    break;
                }
                index = (index + 1) % 4;
            }
            if !found {
                return false; // Single pixel blob
            }

            current.angle += FOUR_DIRECTIONS[index].angle;
            current.r += FOUR_DIRECTIONS[index].r;

            if count >= length {
                return true;
            }
            count += 1;

            if current.angle > max_angle.angle {
                max_angle = current;
            }
            if current.angle < min_angle.angle {
                min_angle = current;
            }
            if current.r > max_r.r {
                max_r = current;
            }
            if current.r < min_r.r {
                min_r = current;
            }
        }

        // Contour too short - erase the blob
        // Note: min_angle normalization not needed since we iterate from min to max
        for a in min_angle.angle..=max_angle.angle {
            let a_idx = self.mod_spokes(a);
            for r in min_r.r..=max_r.r {
                if let Some(pixel) = self.spokes[a_idx].sweep.get_mut(r as usize) {
                    *pixel = pixel.intersection(HistoryPixel::NO_TARGET | HistoryPixel::CONTOUR);
                }
            }
        }

        false
    }

    /// Find contour from an inside point
    /// Moves pol to the edge of the blob
    /// Returns true if blob has minimum contour length
    pub fn find_contour_from_inside(&mut self, doppler: &DopplerState, pol: &mut Polar) -> bool {
        let mut angle = pol.angle;
        let r = pol.r;
        let mut limit = self.spokes_per_revolution as i32 / 8;

        if !self.pix(doppler, angle, r) {
            return false;
        }

        // Move left until we find the edge
        while limit >= 0 && self.pix(doppler, angle, r) {
            angle -= 1;
            limit -= 1;
        }
        angle += 1;
        pol.angle = angle;

        // Check if blob has minimum contour length
        self.multi_pix(doppler, angle, r)
    }

    /// Helper for find_nearest_contour
    fn pix2(&mut self, doppler: &DopplerState, pol: &mut Polar, a: i32, r: i32) -> bool {
        if self.multi_pix(doppler, a, r) {
            pol.angle = a;
            pol.r = r;
            return true;
        }
        false
    }

    /// Search for nearest contour in a square pattern around pol
    pub fn find_nearest_contour(
        &mut self,
        doppler: &DopplerState,
        pol: &mut Polar,
        dist: i32,
    ) -> bool {
        let a = pol.angle;
        let r = pol.r;
        let distance = dist.max(2);
        let factor = self.spokes_per_revolution as f64 / 2.0 / std::f64::consts::PI;

        for j in 1..=distance {
            let dist_r = j;
            let dist_a = (factor / r as f64 * j as f64).max(1.0) as i32;

            // Upper side
            for i in 0..=dist_a {
                if self.pix2(doppler, pol, a - i, r + dist_r) {
                    return true;
                }
                if self.pix2(doppler, pol, a + i, r + dist_r) {
                    return true;
                }
            }

            // Right side
            for i in 0..dist_r {
                if self.pix2(doppler, pol, a + dist_a, r + i) {
                    return true;
                }
                if self.pix2(doppler, pol, a + dist_a, r - i) {
                    return true;
                }
            }

            // Lower side
            for i in 0..=dist_a {
                if self.pix2(doppler, pol, a - i, r - dist_r) {
                    return true;
                }
                if self.pix2(doppler, pol, a + i, r - dist_r) {
                    return true;
                }
            }

            // Left side
            for i in 0..dist_r {
                if self.pix2(doppler, pol, a - dist_a, r + i) {
                    return true;
                }
                if self.pix2(doppler, pol, a - dist_a, r - i) {
                    return true;
                }
            }
        }

        false
    }

    /// Get the full contour from a point on the edge of a blob
    pub fn get_contour(
        &mut self,
        doppler: &DopplerState,
        pol: Polar,
    ) -> Result<(Contour, Polar), ContourError> {
        let mut count = 0;
        let mut current = pol;

        let mut contour = Contour::new();
        contour.max_r = current.r;
        contour.max_angle = current.angle;
        contour.min_r = current.r;
        contour.min_angle = current.angle;

        // Bounds check
        if pol.r as usize >= self.spoke_len() {
            return Err(ContourError::RangeTooHigh);
        }
        if pol.r < 4 {
            return Err(ContourError::RangeTooLow);
        }
        if !self.pix(doppler, pol.angle, pol.r) {
            return Err(ContourError::NoEchoAtStart);
        }

        // Find initial orientation
        let mut index = 0;
        let mut found = false;
        for i in 0..4 {
            if !self.pix(
                doppler,
                current.angle + FOUR_DIRECTIONS[i].angle,
                current.r + FOUR_DIRECTIONS[i].r,
            ) {
                found = true;
                index = i;
                break;
            }
        }
        if !found {
            return Err(ContourError::StartPointNotOnContour);
        }

        index = (index + 1) % 4;

        // Follow the contour
        while count < MAX_CONTOUR_LENGTH {
            index = (index + 3) % 4; // Turn left
            found = false;

            for _ in 0..4 {
                let next = current + FOUR_DIRECTIONS[index];
                if self.pix(doppler, next.angle, next.r) {
                    found = true;
                    current = next;
                    break;
                }
                index = (index + 1) % 4;
            }

            if !found {
                return Err(ContourError::BrokenContour);
            }

            contour.points.push(current);

            // Update bounds
            if current.angle > contour.max_angle {
                contour.max_angle = current.angle;
            }
            if current.angle < contour.min_angle {
                contour.min_angle = current.angle;
            }
            if current.r > contour.max_r {
                contour.max_r = current.r;
            }
            if current.r < contour.min_r {
                contour.min_r = current.r;
            }

            count += 1;

            // Check if we've returned to start
            if current.r == pol.r && current.angle == pol.angle {
                break;
            }
        }

        contour.length = contour.points.len() as i32;

        // Calculate centroid
        let mut result_pol = pol;
        result_pol.angle = self.mod_spokes((contour.max_angle + contour.min_angle) / 2) as i32;
        contour.min_angle = self.mod_spokes(contour.min_angle) as i32;
        contour.max_angle = self.mod_spokes(contour.max_angle) as i32;
        result_pol.r = (contour.max_r + contour.min_r) / 2;
        result_pol.time = self.spokes[self.mod_spokes(result_pol.angle)].time;

        contour.position = result_pol;

        Ok((contour, result_pol))
    }

    /// Find a target at expected position
    pub fn get_target(
        &mut self,
        doppler: &DopplerState,
        pol: Polar,
        dist: i32,
    ) -> Result<(Contour, Polar), ContourError> {
        let mut pol = pol;
        let dist = dist.min(pol.r - 5);

        let contour_found = if self.pix(doppler, pol.angle, pol.r) {
            self.find_contour_from_inside(doppler, &mut pol)
        } else {
            self.find_nearest_contour(doppler, &mut pol, dist)
        };

        if !contour_found {
            return Err(ContourError::NoContourFound);
        }

        self.get_contour(doppler, pol)
    }

    /// Reset pixels of a found target to prevent re-detection
    pub fn reset_pixels(&mut self, contour: &Contour, pos: &Polar, pixels_per_meter: f64) {
        const DISTANCE_BETWEEN_TARGETS: i32 = 30;
        const SHADOW_MARGIN: i32 = 5;
        const TARGET_DISTANCE_FOR_BLANKING_SHADOW: f64 = 6000.0;

        // Clear the blob area
        for a in (contour.min_angle - DISTANCE_BETWEEN_TARGETS)
            ..=(contour.max_angle + DISTANCE_BETWEEN_TARGETS)
        {
            let a_idx = self.mod_spokes(a);
            let spoke_len = self.spokes[a_idx].sweep.len() as i32;

            for r in (contour.min_r - DISTANCE_BETWEEN_TARGETS).max(0)
                ..=(contour.max_r + DISTANCE_BETWEEN_TARGETS).min(spoke_len - 1)
            {
                if let Some(pixel) = self.spokes[a_idx].sweep.get_mut(r as usize) {
                    *pixel = pixel.intersection(HistoryPixel::BACKUP);
                }
            }
        }

        // For larger targets, clear the "shadow" behind them
        let distance_to_radar = pos.r as f64 / pixels_per_meter;
        if contour.length > 20 && distance_to_radar < TARGET_DISTANCE_FOR_BLANKING_SHADOW {
            let mut max_angle = contour.max_angle;
            if contour.min_angle - SHADOW_MARGIN > contour.max_angle + SHADOW_MARGIN {
                max_angle += self.spokes_per_revolution as i32;
            }

            for a in (contour.min_angle - SHADOW_MARGIN)..=(max_angle + SHADOW_MARGIN) {
                let a_idx = self.mod_spokes(a);
                let spoke_len = self.spokes[a_idx].sweep.len();
                let max_r = (4 * contour.max_r as usize).min(spoke_len - 1);

                for r in (contour.max_r as usize)..=max_r {
                    if let Some(pixel) = self.spokes[a_idx].sweep.get_mut(r) {
                        *pixel = pixel.intersection(HistoryPixel::BACKUP);
                    }
                }
            }
        }

        // Draw the contour for visualization
        for p in &contour.points {
            let a_idx = self.mod_spokes(p.angle);
            if let Some(pixel) = self.spokes[a_idx].sweep.get_mut(p.r as usize) {
                pixel.insert(HistoryPixel::CONTOUR);
            }
        }
    }

    /// Update a spoke with new radar data
    pub fn update_spoke(
        &mut self,
        angle: usize,
        data: &[u8],
        time: u64,
        lat: f64,
        lon: f64,
        legend: &Legend,
    ) {
        if angle >= self.spokes.len() {
            return;
        }

        let spoke = &mut self.spokes[angle];
        spoke.time = time;
        spoke.lat = lat;
        spoke.lon = lon;
        spoke.sweep.clear();
        spoke.sweep.resize(data.len(), HistoryPixel::new());

        for (radius, &value) in data.iter().enumerate() {
            if value >= legend.strong_return {
                spoke.sweep[radius] = HistoryPixel::INITIAL;
            }

            if value == legend.doppler_approaching {
                spoke.sweep[radius].insert(HistoryPixel::APPROACHING);
            }

            if value == legend.doppler_receding {
                spoke.sweep[radius].insert(HistoryPixel::RECEDING);
            }
        }
    }

    /// Get own ship position at a given angle
    pub fn get_position_at_angle(&self, angle: i32) -> (f64, f64) {
        let idx = self.mod_spokes(angle);
        (self.spokes[idx].lat, self.spokes[idx].lon)
    }

    /// Get timestamp at a given angle
    pub fn get_time_at_angle(&self, angle: i32) -> u64 {
        let idx = self.mod_spokes(angle);
        self.spokes[idx].time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_pixel_flags() {
        let mut pixel = HistoryPixel::INITIAL;
        assert!(pixel.contains(HistoryPixel::TARGET));
        assert!(pixel.contains(HistoryPixel::BACKUP));

        pixel.insert(HistoryPixel::APPROACHING);
        assert!(pixel.contains(HistoryPixel::APPROACHING));
        assert!(!pixel.contains(HistoryPixel::RECEDING));
    }

    #[test]
    fn test_matches_doppler() {
        let pixel = HistoryPixel::INITIAL | HistoryPixel::APPROACHING;
        assert!(pixel.matches_doppler(&DopplerState::Any));
        assert!(pixel.matches_doppler(&DopplerState::Approaching));
        assert!(!pixel.matches_doppler(&DopplerState::Receding));
        assert!(pixel.matches_doppler(&DopplerState::AnyDoppler));
    }

    #[test]
    fn test_mod_spokes() {
        let buffer = HistoryBuffer::new(2048);
        assert_eq!(buffer.mod_spokes(0), 0);
        assert_eq!(buffer.mod_spokes(2048), 0);
        assert_eq!(buffer.mod_spokes(-1), 2047);
        assert_eq!(buffer.mod_spokes(2049), 1);
    }

    #[test]
    fn test_update_spoke() {
        let mut buffer = HistoryBuffer::new(360);
        let legend = Legend::default();

        let data = vec![0, 50, 100, 255, 254, 0];
        buffer.update_spoke(0, &data, 1000, 51.5, -0.1, &legend);

        assert_eq!(buffer.spokes[0].sweep.len(), 6);
        assert_eq!(buffer.spokes[0].time, 1000);

        // Value 100 >= strong_return (64)
        assert!(buffer.spokes[0].sweep[2].contains(HistoryPixel::TARGET));

        // Value 255 is doppler approaching
        assert!(buffer.spokes[0].sweep[3].contains(HistoryPixel::APPROACHING));

        // Value 254 is doppler receding
        assert!(buffer.spokes[0].sweep[4].contains(HistoryPixel::RECEDING));
    }
}
