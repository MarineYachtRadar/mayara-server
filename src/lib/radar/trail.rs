use cartesian::PolarToCartesianLookup;
use ndarray::{Array2, s};

mod cartesian;
use super::settings::{ControlError, ControlId, ControlValue, SharedControls};
use super::{RadarError, RadarInfo};
use crate::TargetMode;
use crate::protos::RadarMessage::radar_message::Spoke;
use crate::radar::target::{METERS_PER_DEGREE_LATITUDE, meters_per_degree_longitude};
use crate::radar::trail::cartesian::PointInt;
use crate::radar::{BLOB_HISTORY_COLORS, GeoPosition, Legend, SpokeBearing};

const MARGIN_I16: i16 = 100;
const MARGIN_USIZE: usize = MARGIN_I16 as usize;

pub(crate) struct GeoPositionPixels {
    lat: i16,
    lon: i16,
}

pub(crate) struct TrailBuffer {
    target_mode: TargetMode,
    legend: Legend,
    spokes_per_revolution: usize,
    max_spoke_len: usize,
    trail_size: i16,
    motion_true: bool,
    position: GeoPosition,
    position_difference: GeoPosition, // Fraction of a pixel expressed in lat/lon for True Motion Target Trails
    position_offset: GeoPositionPixels, // Offset of the trails image in pixels
    cartesian_lookup: PolarToCartesianLookup,
    true_trails: Box<Array2<u8>>,
    true_trails_offset: PointInt,
    relative_trails: Vec<u16>,
    trail_length_ms: u32,
    rotation_speed_ms: u32,
    minimal_legend_value: u8,
    previous_range: u32,
    pixels_per_meter: f64,
    have_heading: bool,
}

impl TrailBuffer {
    pub fn new(info: &RadarInfo) -> Self {
        let spokes_per_revolution = info.spokes_per_revolution as usize;
        let max_spoke_len = info.max_spoke_len as usize;
        let trail_size: i16 = info.max_spoke_len as i16 * 2 + MARGIN_I16 * 2;
        let cartesian_lookup = PolarToCartesianLookup::new(
            info.spokes_per_revolution as usize,
            info.max_spoke_len as usize,
        );

        let legend = info.get_legend();
        let mut minimal_legend_value = 0;
        if let Some(control) = info.controls.get(&ControlId::DopplerTrailsOnly)
            && let Some(value) = control.value
        {
            let value = value > 0.;
            minimal_legend_value = Self::compute_minimal_legend_value(&legend, value);
        }

        TrailBuffer {
            target_mode: TargetMode::Trails,
            legend,
            spokes_per_revolution,
            max_spoke_len,
            trail_size,
            motion_true: false,
            position: GeoPosition::new(0., 0.),
            position_difference: GeoPosition { lat: 0., lon: 0. },
            position_offset: GeoPositionPixels { lat: 0, lon: 0 },
            cartesian_lookup,
            true_trails: Box::new(Array2::<u8>::zeros((
                trail_size as usize,
                trail_size as usize,
            ))),
            true_trails_offset: PointInt { x: 0, y: 0 },
            relative_trails: vec![0; spokes_per_revolution * max_spoke_len],
            trail_length_ms: 0,
            rotation_speed_ms: 0,
            minimal_legend_value,
            previous_range: 0,
            pixels_per_meter: 0.0,
            have_heading: false,
        }
    }

    pub fn set_control_value(
        &mut self,
        controls: &SharedControls,
        cv: &ControlValue,
    ) -> Result<(), RadarError> {
        let mut reply = match cv.id {
            ControlId::ClearTrails => {
                self.clear();
                return Ok(());
            }
            ControlId::DopplerTrailsOnly => {
                let v = cv.as_value()?;
                let r = controls.set_value(&cv.id, v);
                if r.is_ok() {
                    let value = controls.get(&cv.id).unwrap().as_u16().unwrap_or(0) > 0;
                    self.set_doppler_trail_only(value);
                }
                return r.map(|_| ()).map_err(RadarError::ControlError);
            }
            ControlId::TargetTrails => {
                let v = cv.as_value()?;
                let r = controls.set_value(&cv.id, v);
                if r.is_ok() {
                    let value = controls.get(&cv.id).unwrap().as_u16().unwrap_or(0);
                    self.set_relative_trails_length(value);
                }
                return r.map(|_| ()).map_err(RadarError::ControlError);
            }
            ControlId::TrailsMotion => {
                let true_motion = cv.as_bool()?;
                return self
                    .set_trails_mode(true_motion)
                    .map_err(RadarError::ControlError);
            }
            _ => Err(RadarError::CannotSetControlId(cv.id)),
        };

        // If we are still here, we still need to set the control value
        if reply.is_ok() {
            let value = cv.as_value()?;
            reply = controls
                .set_value(&cv.id, value)
                .map(|_| ())
                .map_err(RadarError::ControlError);
        }
        reply
    }

    pub fn set_trails_mode(&mut self, value: bool) -> Result<(), ControlError> {
        if value {
            if !self.have_heading {
                return Err(ControlError::NoHeading(ControlId::TrailsMotion, "True"));
            }
            if crate::navdata::get_radar_position().is_none() {
                return Err(ControlError::NoPosition(ControlId::TrailsMotion, "True"));
            }
        }
        self.motion_true = value;
        log::debug!("Trails motion set to {:?}", value);
        Ok(())
    }

    pub fn set_relative_trails_length(&mut self, control_value: u16) {
        let milli_seconds: u32 = match control_value {
            0 => 0,
            1 => 15,
            2 => 30,
            3 => 60,
            4 => 3 * 60,
            5 => 5 * 60,
            6 => 10 * 60,
            _ => {
                return;
            }
        } * 1000;

        log::debug!(
            "Trails length {} -> {} ms",
            self.trail_length_ms,
            milli_seconds
        );

        self.trail_length_ms = milli_seconds;
    }

    pub fn set_rotation_speed(&mut self, ms: u32) {
        log::debug!("trail: set_rotation_speed({})", ms);
        self.rotation_speed_ms = ms;
    }

    pub fn update_trails(
        &mut self,
        spoke: &mut Spoke,
        _legend: &Legend,
        _controls: &SharedControls,
    ) {
        if self.target_mode == TargetMode::None {
            return;
        }

        if spoke.range != self.previous_range && spoke.range != 0 {
            if self.previous_range != 0 {
                let zoom_factor = self.previous_range as f64 / spoke.range as f64;
                self.zoom_relative_trails(zoom_factor);
            }
            self.previous_range = spoke.range;
            self.pixels_per_meter = spoke.data.len() as f64 / spoke.range as f64;
        }

        if let Some(bearing) = spoke.bearing {
            self.have_heading = true;
            self.update_true_trails(spoke.range, bearing as u16, &mut spoke.data);
        } else {
            self.have_heading = false;
        }

        self.update_relative_trails(spoke.angle as u16, &mut spoke.data);
    }

    fn update_true_trails(&mut self, range: u32, bearing: SpokeBearing, data: &mut [u8]) {
        if self.trail_length_ms == 0 || self.rotation_speed_ms == 0 {
            return;
        }

        self.update_trail_position(range, data.len());

        let max_trail_value = (self.trail_length_ms / self.rotation_speed_ms) as u8;

        let mut radius = 0;

        while radius < data.len() - 1 {
            //  len - 1 : no trails on range circle
            let mut point = *self
                .cartesian_lookup
                .get_point_int(bearing as usize, radius);

            point.x += self.trail_size / 2 + self.true_trails_offset.x;
            point.y += self.trail_size / 2 + self.true_trails_offset.y;

            if point.x >= 0
                && point.x < self.trail_size
                && point.y >= 0
                && point.y < self.trail_size
            {
                let trail = &mut self.true_trails[[point.x as usize, point.y as usize]];
                // when ship moves north, offset.lat > 0. Add to move trails image in opposite direction
                // when ship moves east, offset.lon > 0. Add to move trails image in opposite direction
                if data[radius] >= self.minimal_legend_value
                    && data[radius] < self.legend.history_start
                {
                    *trail = 1;
                } else if *trail > 0 {
                    *trail = trail.wrapping_add(1); // Yes, we want overflow here after 65535 rotations
                }

                let trail = *trail;
                if self.motion_true && data[radius] == 0 && trail > 0 && trail < max_trail_value {
                    let index: u8 = (trail * BLOB_HISTORY_COLORS / max_trail_value)
                        .clamp(1, BLOB_HISTORY_COLORS);

                    data[radius] = self.legend.history_start + index - 1;
                }
                radius += 1;
            }
        }
    }

    fn update_trail_position(&mut self, range: u32, data_length: usize) {
        // When position changes the trail image is not moved, only the pointer to the center
        // of the image (offset) is changed.
        // So we move the image around within the m_trails.true_trails buffer (by moving the pointer).
        // But when there is no room anymore (margin used) the whole trails image is shifted
        // and the offset is reset
        if self.position_offset.lon >= MARGIN_I16
            || self.position_offset.lon <= -MARGIN_I16
            || self.position_offset.lat >= MARGIN_I16
            || self.position_offset.lat <= -MARGIN_I16
        {
            log::debug!(
                "offset lat {} / lon {} larger than {}",
                self.position_offset.lat,
                self.position_offset.lon,
                MARGIN_I16
            );
            // move the image in the offset direction, to move it closer to the center
            self.shift_image_lat_to_center();
            self.shift_image_lon_to_center();
        }
        let pixels_per_meter: f64 = data_length as f64 / range as f64;
        if self.pixels_per_meter == 0. {
            self.clear();
            if pixels_per_meter == 0. {
                return;
            }
            self.pixels_per_meter = pixels_per_meter;
        } else if self.pixels_per_meter != pixels_per_meter && self.pixels_per_meter != 0. {
            // zoom trails
            let zoom_factor = pixels_per_meter / self.pixels_per_meter;

            if !(0.25..=4.00).contains(&zoom_factor) {
                self.clear();
                return;
            }
            self.pixels_per_meter = pixels_per_meter;
            // center the image before zooming
            // otherwise the offset might get too large
            self.shift_image_lat_to_center();
            self.shift_image_lon_to_center();
            self.zoom_true_trails(zoom_factor);
        }

        if let Some(position) = crate::navdata::get_radar_position() {
            // Did the ship move? No, return.
            if self.position == position {
                return;
            }
            // Check the movement of the ship
            let dif_lat = position.lat - self.position.lat; // going north is positive
            let dif_lon = position.lon - self.position.lon; // moving east is positive

            self.position = position;
            // get (floating point) shift of the ship in radar pixels
            let fshift_lat = dif_lat * METERS_PER_DEGREE_LATITUDE * pixels_per_meter;
            let fshift_lon =
                dif_lon * meters_per_degree_longitude(&position.lat) * pixels_per_meter;

            // Get the integer pixel shift, first add previous rounding error
            let shift = GeoPositionPixels {
                lat: (fshift_lat + self.position_difference.lat) as i16,
                lon: (fshift_lon + self.position_difference.lon) as i16,
            };

            // save the rounding fraction and apply it next time
            self.position_difference.lat =
                fshift_lat + self.position_difference.lat - shift.lat as f64;
            self.position_difference.lon =
                fshift_lon + self.position_difference.lon - shift.lon as f64;

            if shift.lat >= MARGIN_I16
                || shift.lat <= -MARGIN_I16
                || shift.lon >= MARGIN_I16
                || shift.lon <= -MARGIN_I16
            {
                // huge shift, reset trails

                log::debug!(
                    "Large movement trails reset, lat {} / {}",
                    shift.lat,
                    shift.lon
                );
                self.clear();
                return;
            }

            // offset lon too large: shift image
            if (self.position_offset.lon + shift.lon).abs() >= MARGIN_I16 {
                self.shift_image_lon_to_center();
            }

            // offset lat too large: shift image in lat direction
            if (self.position_offset.lat + shift.lat).abs() >= MARGIN_I16 {
                self.shift_image_lat_to_center();
            }
            // apply the shifts to the offset
            self.position_offset.lat += shift.lat;
            self.position_offset.lon += shift.lon;
        }
    }

    // shifts the true trails image in lat direction to center
    fn shift_image_lat_to_center(&mut self) {
        if self.position_offset.lat >= MARGIN_I16 || self.position_offset.lat <= -MARGIN_I16 {
            // abs not ok
            log::debug!("offset lat too large {}", self.position_offset.lat);
            self.clear();
            return;
        }

        let n = self.position_offset.lat;

        let (to_start, to_end, from_start, from_end, zero_start, zero_end) = if n > 0 {
            let n = n as usize;
            (
                0_usize,
                self.true_trails.nrows() - n,
                n,
                self.true_trails.nrows(),
                self.true_trails.nrows() - n,
                n,
            )
        } else {
            let n = -n as usize;
            (
                n,
                self.true_trails.nrows(),
                0_usize,
                self.true_trails.nrows() - n,
                0,
                n,
            )
        };

        let from_slice = self
            .true_trails
            .slice(s![from_start..from_end, ..])
            .to_owned();
        let mut to_slice = self.true_trails.slice_mut(s![to_start..to_end, ..]);
        to_slice.assign(&from_slice);

        // Fill the remaining rows with zeros using slices
        let mut zero_slice = self.true_trails.slice_mut(s![zero_start..zero_end, ..]);
        zero_slice.fill(0);
        self.position_offset.lat = 0;
    }

    // shifts the true trails image in lon direction to center
    fn shift_image_lon_to_center(&mut self) {
        if self.position_offset.lon >= MARGIN_I16 || self.position_offset.lon <= -MARGIN_I16 {
            // abs no good
            log::debug!("offset lon too large {}", self.position_offset.lon);
            self.clear();
            return;
        }

        let n = self.position_offset.lon;

        let (to_start, to_end, from_start, from_end, zero_start, zero_end) = if n > 0 {
            let n = n as usize;
            (
                0_usize,
                self.true_trails.ncols() - n,
                n,
                self.true_trails.ncols(),
                self.true_trails.ncols() - n,
                n,
            )
        } else {
            let n = -n as usize;
            (
                n,
                self.true_trails.ncols(),
                0_usize,
                self.true_trails.ncols() - n,
                0,
                n,
            )
        };

        let from_slice = self
            .true_trails
            .slice(s![.., from_start..from_end])
            .to_owned();
        let mut to_slice = self.true_trails.slice_mut(s![.., to_start..to_end]);
        to_slice.assign(&from_slice);

        // Fill the remaining rows with zeros using slices
        let mut zero_slice = self.true_trails.slice_mut(s![.., zero_start..zero_end]);
        zero_slice.fill(0);
        self.position_offset.lon = 0;
    }

    // Zooms the trailbuffer (containing image of true trails) in and out
    // This version assumes m_offset.lon and m_offset.lat to be zero (earlier versions did zoom offset as well)
    // zoom_factor > 1 -> zoom in, enlarge image
    fn zoom_true_trails(&mut self, zoom_factor: f64) {
        let trail_size = self.trail_size as usize;
        let mut new_trails = Box::new(Array2::<u8>::zeros((trail_size, trail_size)));

        // zoom true trails
        for i in MARGIN_USIZE..trail_size - MARGIN_USIZE {
            let index_i = ((i - trail_size / 2) as f64 * zoom_factor) as i16 + self.trail_size / 2;
            if index_i >= self.trail_size - 1 {
                // allow adding an additional pixel later
                break;
            }
            if index_i < 0 {
                continue;
            }
            for j in MARGIN_USIZE..trail_size - MARGIN_USIZE {
                let index_j: i16 =
                    ((j - trail_size / 2) as f64 * zoom_factor) as i16 + self.trail_size / 2;
                if index_j >= self.trail_size - 1 {
                    break;
                }
                if index_j < 0 {
                    continue;
                }
                let pixel = self.true_trails[[i, j]];
                if pixel != 0 {
                    let index_i = index_i as usize;
                    let index_j = index_j as usize;
                    // many to one mapping, prevent overwriting trails with 0
                    new_trails[[index_i, index_j]] = pixel;
                    if zoom_factor > 1.2 {
                        // add an extra pixel in the y direction
                        new_trails[[index_i, index_j + 1]] = pixel;
                        if zoom_factor > 1.6 {
                            // also add pixels in the x direction
                            new_trails[[index_i + 1, index_j]] = pixel;
                            new_trails[[index_i + 1, index_j + 1]] = pixel;
                        }
                    }
                }
            }
        }
        self.true_trails = new_trails;
    }

    pub fn update_relative_trails(&mut self, angle: SpokeBearing, data: &mut Vec<u8>) {
        if angle == 0 {
            log::trace!(
                "angle = {}, trails_length_ms = {}, rotation_speed_ms = {}",
                angle,
                self.trail_length_ms,
                self.rotation_speed_ms
            );
        }
        if self.trail_length_ms == 0 || self.rotation_speed_ms == 0 {
            return;
        }
        let max_trail_value = (self.trail_length_ms / self.rotation_speed_ms) as u16;

        let trail = &mut self.relative_trails
            [angle as usize * self.max_spoke_len..(angle + 1) as usize * self.max_spoke_len];

        let mut radius = 0;

        if angle == 0 {
            log::trace!("Spoke before trails: {:?}", data);
        }

        while radius < data.len() {
            if data[radius] >= self.minimal_legend_value && data[radius] < self.legend.history_start
            {
                trail[radius] = 1;
            } else if trail[radius] > 0 {
                trail[radius] = trail[radius].wrapping_add(1); // Yes, we want overflow here after 65535 rotations
            }

            if !self.motion_true
                && data[radius] <= 1
                && trail[radius] > 0
                && trail[radius] < max_trail_value
            {
                let index = ((trail[radius] * BLOB_HISTORY_COLORS as u16 / max_trail_value) as u8)
                    .clamp(1, BLOB_HISTORY_COLORS);

                data[radius] = self.legend.history_start + index - 1;
            }
            radius += 1;
        }
        while radius < self.max_spoke_len {
            trail[radius] = 0;
            radius += 1;
        }

        if angle == 0 {
            log::trace!("Trail after trails: {:?}", trail);
            log::trace!("Spoke after trails: {:?}", data);
        }
    }

    // zoom_factor > 1 -> zoom in, enlarge image
    fn zoom_relative_trails(&mut self, zoom_factor: f64) {
        let mut new_trail = vec![0; self.max_spoke_len];
        for spoke in 0..self.spokes_per_revolution {
            // Reset the interpolation start-of-run cursor on every spoke;
            // letting it carry over across spokes lets a later spoke's
            // first `index_new` end up less than `index_prev`, which the
            // `.fill(...)` slice would panic on.
            let mut index_prev = 0;
            {
                let trail = &self.relative_trails
                    [spoke * self.max_spoke_len..(spoke + 1) * self.max_spoke_len];

                for (j, &t) in trail.iter().enumerate() {
                    let index_new = (j as f64 * zoom_factor) as usize;
                    if index_new >= self.max_spoke_len {
                        break;
                    }
                    if t != 0 && index_prev <= index_new {
                        new_trail[index_prev..=index_new].fill(t);
                    }
                    index_prev = index_new + 1;
                }
            }
            self.relative_trails[spoke * self.max_spoke_len..(spoke + 1) * self.max_spoke_len]
                .copy_from_slice(&new_trail);

            new_trail.fill(0);
        }
    }

    pub fn clear(&mut self) {
        self.true_trails.fill(0);
        self.relative_trails.fill(0);
    }

    fn compute_minimal_legend_value(legend: &Legend, doppler_only: bool) -> u8 {
        if doppler_only && let Some((start, _)) = legend.doppler_approaching {
            start
        } else {
            legend.strong_return
        }
    }

    fn set_doppler_trail_only(&mut self, v: bool) {
        self.minimal_legend_value = Self::compute_minimal_legend_value(&self.legend, v);
    }
}
