#![allow(dead_code, unused_variables)]

use std::{
    cmp::{max, min},
    collections::HashMap,
    f64::consts::PI,
    sync::{Arc, RwLock},
};
use strum::{EnumIter, IntoEnumIterator};

use mayara_core::arpa::{
    ContourError, DopplerState, HistoryPixel, KalmanFilter, LocalPosition, Polar,
    MAX_CONTOUR_LENGTH, MIN_CONTOUR_LENGTH,
};
// Re-export constants for other modules
pub use mayara_core::arpa::{METERS_PER_DEGREE_LATITUDE, MS_TO_KN};
use ndarray::Array2;

use crate::{
    navdata, protos::RadarMessage::radar_message::Spoke, radar::NAUTICAL_MILE_F64,
    settings::ControlError, Session,
};

use super::{GeoPosition, Legend, RadarInfo};

mod arpa;

const MAX_LOST_COUNT: i32 = 12; // number of sweeps that target can be missed before it is set to lost
const MAX_DETECTION_SPEED_KN: f64 = 40.;

pub const KN_TO_MS: f64 = NAUTICAL_MILE_F64 / 3600.;

const TODO_ROTATION_SPEED_MS: i32 = 2500;
const TODO_TARGET_AGE_TO_MIXER: u32 = 5;

// Re-export these for use in other modules
pub use mayara_core::arpa::meters_per_degree_longitude;

#[derive(Debug, Clone)]
struct ExtendedPosition {
    pos: GeoPosition,
    dlat_dt: f64, // m / sec
    dlon_dt: f64, // m / sec
    time: u64,    // millis
    speed_kn: f64,
    sd_speed_kn: f64, // standard deviation of the speed in knots
}

impl ExtendedPosition {
    fn new(
        pos: GeoPosition,
        dlat_dt: f64,
        dlon_dt: f64,
        time: u64,
        speed_kn: f64,
        sd_speed_kn: f64,
    ) -> Self {
        Self {
            pos,
            dlat_dt,
            dlon_dt,
            time,
            speed_kn,
            sd_speed_kn,
        }
    }
    fn empty() -> Self {
        Self::new(GeoPosition::new(0., 0.), 0., 0., 0, 0., 0.)
    }
}

// We try to find each target three times, with different conditions each time
#[derive(Debug, Clone, Copy, PartialEq, EnumIter)]
enum Pass {
    First,
    Second,
    Third,
}

#[derive(Debug, Clone, PartialEq)]
enum TargetStatus {
    Acquire0,    // Under acquisition, first seen, no contour yet
    Acquire1,    // Under acquisition, first contour found
    Acquire2,    // Under acquisition, speed and course known
    ACQUIRE3,    // Under acquisition, speed and course known, next time active
    ACTIVE,      // Active target
    LOST,        // Lost target
    ForDeletion, // Target to be deleted
}

#[derive(Debug, Clone, PartialEq)]
enum RefreshState {
    NotFound,
    Found,
    OutOfScope,
}

// Use DopplerState from core - alias for backward compatibility in this file
type Doppler = DopplerState;

// Use FOUR_DIRECTIONS from core
use mayara_core::arpa::FOUR_DIRECTIONS;

// Server-specific HistorySpoke that includes GeoPosition (not in core)
#[derive(Debug, Clone)]
struct ServerHistorySpoke {
    sweep: Vec<HistoryPixel>,
    time: u64,
    pos: GeoPosition,
}

#[derive(Debug, Clone)]
struct HistorySpokes {
    spokes: Box<Vec<ServerHistorySpoke>>,
    stationary_layer: Option<Box<Array2<u8>>>,
}
#[derive(Debug, Clone)]
pub struct TargetBuffer {
    session: Session,
    setup: TargetSetup,
    next_target_id: usize,
    history: HistorySpokes,
    targets: Arc<RwLock<HashMap<usize, ArpaTarget>>>,

    arpa_via_doppler: bool,

    m_clear_contours: bool,
    m_auto_learn_state: i32,

    // Average course
    course: f64,
    course_weight: u16,
    course_samples: u16,

    // If we have just received angle <n>
    // then we look for refreshed targets in <n + spokes_per_revolution/4> .. <n +
    // spokes_per_revolution / 2>
    scanned_angle: i32,
    // and we scan for new targets at <n + 3/4 * spokes_per_revolution> (SCAN_FOR_NEW_PERCENTAGE)
    refreshed_angle: i32,
}

const REFRESH_START_PERCENTAGE: i32 = 25;
const REFRESH_END_PERCENTAGE: i32 = 50;
const SCAN_FOR_NEW_PERCENTAGE: i32 = 75;

#[derive(Debug, Clone)]
pub(self) struct TargetSetup {
    radar_id: usize,
    spokes_per_revolution: i32,
    spokes_per_revolution_f64: f64,
    spoke_len: i32,
    have_doppler: bool,
    pixels_per_meter: f64,
    rotation_speed_ms: u32,
    stationary: bool,
}

// Server-local Contour - slightly different field names from core
// We keep this for compatibility with existing code, but it wraps core's Contour
#[derive(Debug, Clone)]
struct ServerContour {
    length: i32,
    min_angle: i32,
    max_angle: i32,
    min_r: i32,
    max_r: i32,
    position: Polar,
    contour: Vec<Polar>, // Named 'contour' here vs 'points' in core
}

// Use ContourError from core
type Error = ContourError;

#[derive(Debug, Clone)]
struct Sector {
    start_angle: i32,
    end_angle: i32,
}

impl Sector {
    fn new(start_angle: i32, end_angle: i32) -> Self {
        Sector {
            start_angle,
            end_angle,
        }
    }
}

#[derive(Debug, Clone)]
struct ArpaTarget {
    m_status: TargetStatus,

    m_average_contour_length: i32,
    m_small_fast: bool,
    m_previous_contour_length: i32,
    m_lost_count: i32,
    m_refresh_time: u64,
    m_automatic: bool,
    m_radar_pos: GeoPosition,
    m_course: f64,
    m_stationary: i32,
    m_doppler_target: Doppler,
    pub m_refreshed: RefreshState,
    m_target_id: usize,
    m_transferred_target: bool,
    m_kalman: KalmanFilter,
    contour: ServerContour,
    m_total_pix: u32,
    m_approaching_pix: u32,
    m_receding_pix: u32,
    have_doppler: bool,
    pub position: ExtendedPosition,
    expected: Polar,
    pub age_rotations: u32,
}

impl ServerHistorySpoke {
    fn new(sweep: Vec<HistoryPixel>, time: u64, pos: GeoPosition) -> Self {
        Self { sweep, time, pos }
    }
}

impl HistorySpokes {
    fn new(session: Session, spokes_per_revolution: i32, spoke_len: i32) -> Self {
        let stationary = session.read().unwrap().args.stationary;
        log::debug!(
            "creating HistorySpokes ({} x {}) stationary: {}",
            spokes_per_revolution,
            spoke_len,
            stationary
        );
        Self {
            spokes: Box::new(vec![
                ServerHistorySpoke::new(
                    vec![HistoryPixel::INITIAL; 0],
                    0,
                    GeoPosition::new(0., 0.)
                );
                spokes_per_revolution as usize
            ]),
            stationary_layer: if stationary {
                Some(Box::new(Array2::<u8>::zeros((
                    spokes_per_revolution as usize,
                    spoke_len as usize,
                ))))
            } else {
                None
            },
        }
    }

    pub fn mod_spokes(&self, angle: i32) -> usize {
        (angle as usize + self.spokes.len()) % self.spokes.len()
    }

    pub fn pix(&self, doppler: &Doppler, ang: i32, rad: i32) -> bool {
        let rad = rad as usize;
        if rad >= self.spokes[0].sweep.len() || rad < 3 {
            return false;
        }
        let angle = self.mod_spokes(ang);
        if let Some(layer) = &self.stationary_layer {
            if layer[[angle, rad]] != 0 {
                return false;
            }
        }
        let history = self.spokes[angle]
            .sweep
            .get(rad)
            .unwrap_or(&HistoryPixel::INITIAL);
        let target = history.contains(HistoryPixel::TARGET); // above threshold bit
        let backup = history.contains(HistoryPixel::BACKUP); // backup bit does not get cleared when target is refreshed
        let approaching = history.contains(HistoryPixel::APPROACHING); // this is Doppler approaching bit
        let receding = history.contains(HistoryPixel::RECEDING); // this is Doppler receding bit

        match doppler {
            Doppler::Any => target,
            Doppler::NoDoppler => target && !approaching && !receding,
            Doppler::Approaching => approaching,
            Doppler::Receding => receding,
            Doppler::AnyDoppler => approaching || receding,
            Doppler::NotReceding => target && !receding,
            Doppler::NotApproaching => target && !approaching,
            Doppler::AnyPlus => backup,
        }
    }

    fn multi_pix(&mut self, doppler: &Doppler, ang: i32, rad: i32) -> bool {
        // checks if the blob has a contour of at least length pixels
        // pol must start on the contour of the blob
        // false if not
        // if false clears out pixels of the blob in hist

        if !self.pix(doppler, ang, rad) {
            return false;
        }
        let length = MIN_CONTOUR_LENGTH;
        let start = Polar::new(ang as i32, rad as i32, 0);

        let mut current = start; // the 4 possible translations to move from a point on the contour to the next

        let mut max_angle = current;
        let mut min_angle = current;
        let mut max_r = current;
        let mut min_r = current;
        let mut count = 0;
        let mut found = false;

        // first find the orientation of border point p
        let index = {
            let mut index = 0;
            for i in 0..4 {
                if !self.pix(
                    doppler,
                    current.angle + FOUR_DIRECTIONS[i].angle,
                    current.r + FOUR_DIRECTIONS[i].r,
                ) {
                    found = true;
                    break;
                }
                index += 1;
            }
            if !found {
                return false; // single pixel blob
            }
            index
        };
        let mut index = (index + 1) % 4; // determines starting direction
        found = false;

        while current.r != start.r || current.angle != start.angle || count == 0 {
            // try all translations to find the next point
            // start with the "left most" translation relative to the
            // previous one
            index = (index + 3) % 4; // we will turn left all the time if possible
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
                return false; // no next point found (this happens when the blob consists of one single pixel)
            } // next point found
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
        } // contour length is less than m_min_contour_length
          // before returning false erase this blob so we do not have to check this one again
        if min_angle.angle < 0 {
            min_angle.angle += self.spokes.len() as i32;
            max_angle.angle += self.spokes.len() as i32;
        }
        for a in min_angle.angle..=max_angle.angle {
            let a_normalized = self.mod_spokes(a);
            for r in min_r.r..=max_r.r {
                self.spokes[a_normalized].sweep[r as usize] = self.spokes[a_normalized].sweep
                    [r as usize]
                    .intersection(HistoryPixel::NO_TARGET | HistoryPixel::CONTOUR);
            }
        }
        return false;
    }

    // moves pol to contour of blob
    // true if success
    // false when failed
    fn find_contour_from_inside(&mut self, doppler: &Doppler, pol: &mut Polar) -> bool {
        let mut ang = pol.angle;
        let rad = pol.r;
        let mut limit = self.spokes.len() as i32 / 8;

        if !self.pix(doppler, ang, rad) {
            return false;
        }
        while limit >= 0 && self.pix(doppler, ang, rad) {
            ang -= 1;
            limit -= 1;
        }
        ang += 1;
        pol.angle = ang;

        // return true if the blob has the required min contour length
        self.multi_pix(doppler, ang, rad)
    }

    fn pix2(&mut self, doppler: &Doppler, pol: &mut Polar, a: i32, r: i32) -> bool {
        if self.multi_pix(doppler, a, r) {
            pol.angle = a;
            pol.r = r;
            return true;
        }
        return false;
    }

    /// make a search pattern along a square
    /// returns the position of the nearest blob found in pol
    /// dist is search radius (1 more or less) in radial pixels
    fn find_nearest_contour(&mut self, doppler: &Doppler, pol: &mut Polar, dist: i32) -> bool {
        let a = pol.angle;
        let r = pol.r;
        let distance = max(dist, 2);
        let factor: f64 = self.spokes.len() as f64 / 2.0 / PI;

        for j in 1..=distance {
            let dist_r = j;
            let dist_a = max((factor / r as f64 * j as f64) as i32, 1);
            // search starting from the middle
            for i in 0..=dist_a {
                // "upper" side
                if self.pix2(doppler, pol, a - i, r + dist_r) {
                    return true;
                }
                if self.pix2(doppler, pol, a + i, r + dist_r) {
                    return true;
                }
            }
            for i in 0..dist_r {
                // "right hand" side
                if self.pix2(doppler, pol, a + dist_a, r + i) {
                    return true;
                }
                if self.pix2(doppler, pol, a + dist_a, r - i) {
                    return true;
                }
            }
            for i in 0..=dist_a {
                // "lower" side
                if self.pix2(doppler, pol, a - i, r - dist_r) {
                    return true;
                }
                if self.pix2(doppler, pol, a + i, r - dist_r) {
                    return true;
                }
            }
            for i in 0..dist_r {
                // "left hand" side
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

    /**
     * Find a contour from the given start position on the edge of a blob.
     *
     * Follows the contour in a clockwise manner.
     *
     *
     */
    fn get_contour(
        &mut self,
        doppler: &Doppler,
        pol: Polar,
    ) -> Result<(ServerContour, Polar), Error> {
        let mut pol = pol;
        let mut count = 0;
        let mut current = pol;
        let mut next = current;

        let mut succes = false;
        let mut index = 0;

        let mut contour = ServerContour::new();
        contour.max_r = current.r;
        contour.max_angle = current.angle;
        contour.min_r = current.r;
        contour.min_angle = current.angle;

        // check if p inside blob
        if pol.r as usize >= self.spokes.len() {
            return Err(Error::RangeTooHigh);
        }
        if pol.r < 4 {
            return Err(Error::RangeTooLow);
        }
        if !self.pix(doppler, pol.angle, pol.r) {
            return Err(Error::NoEchoAtStart);
        }

        // first find the orientation of border point p
        for i in 0..4 {
            index = i;
            if !self.pix(
                doppler,
                current.angle + FOUR_DIRECTIONS[index].angle,
                current.r + FOUR_DIRECTIONS[index].r,
            ) {
                succes = true;
                break;
            }
        }
        if !succes {
            return Err(Error::StartPointNotOnContour);
        }
        index = (index + 1) % 4; // determines starting direction

        succes = false;
        while count < MAX_CONTOUR_LENGTH {
            // try all translations to find the next point
            // start with the "left most" translation relative to the previous one
            index = (index + 3) % 4; // we will turn left all the time if possible
            for _i in 0..4 {
                next = current + FOUR_DIRECTIONS[index];
                if self.pix(doppler, next.angle, next.r) {
                    succes = true; // next point found

                    break;
                }
                index = (index + 1) % 4;
            }
            if !succes {
                return Err(Error::BrokenContour);
            }
            // next point found
            current = next;
            if count < MAX_CONTOUR_LENGTH - 1 {
                contour.contour.push(current);
            } else if count == MAX_CONTOUR_LENGTH - 1 {
                contour.contour.push(current);
                contour.contour.push(pol); // shortcut to the beginning for drawing the contour
            }
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
        }
        contour.length = contour.contour.len() as i32;

        //  CalculateCentroid(*target);    we better use the real centroid instead of the average, TODO

        pol.angle = self.mod_spokes((contour.max_angle + contour.min_angle) / 2) as i32;
        contour.min_angle = self.mod_spokes(contour.min_angle) as i32;
        contour.max_angle = self.mod_spokes(contour.max_angle) as i32;
        pol.r = (contour.max_r + contour.min_r) / 2;
        pol.time = self.spokes[pol.angle as usize].time;

        // TODO        self.m_radar_pos = buffer.history.spokes[pol.angle as usize].pos;

        return Ok((contour, pol));
    }

    fn get_target(
        &mut self,
        doppler: &Doppler,
        pol: Polar,
        dist1: i32,
    ) -> Result<(ServerContour, Polar), Error> {
        let mut pol = pol;

        // general target refresh

        let dist = min(dist1, pol.r - 5);

        let contour_found = if self.pix(doppler, pol.angle, pol.r) {
            self.find_contour_from_inside(doppler, &mut pol)
        } else {
            self.find_nearest_contour(doppler, &mut pol, dist)
        };
        if !contour_found {
            return Err(Error::NoContourFound);
        }
        self.get_contour(doppler, pol)
    }

    //
    // resets the pixels of the current blob (plus DISTANCE_BETWEEN_TARGETS) so that blob will not be found again in the same sweep
    // We not only reset the blob but all pixels in a radial "square" covering the blob
    fn reset_pixels(&mut self, contour: &ServerContour, pos: &Polar, pixels_per_meter: &f64) {
        const DISTANCE_BETWEEN_TARGETS: i32 = 30;
        const SHADOW_MARGIN: i32 = 5;
        const TARGET_DISTANCE_FOR_BLANKING_SHADOW: f64 = 6000.; // 6 km

        for a in contour.min_angle - DISTANCE_BETWEEN_TARGETS
            ..=contour.max_angle + DISTANCE_BETWEEN_TARGETS
        {
            let a = self.mod_spokes(a);
            for r in max(contour.min_r - DISTANCE_BETWEEN_TARGETS, 0)
                ..=min(
                    contour.max_r + DISTANCE_BETWEEN_TARGETS,
                    self.spokes[0].sweep.len() as i32 - 1,
                )
            {
                self.spokes[a].sweep[r as usize] =
                    self.spokes[a].sweep[r as usize].intersection(HistoryPixel::BACKUP);
                // also clear both Doppler bits
            }
        }

        let distance_to_radar = pos.r as f64 / pixels_per_meter;
        // For larger targets clear the "shadow" of the target until 4 * r ????
        if contour.length > 20 && distance_to_radar < TARGET_DISTANCE_FOR_BLANKING_SHADOW {
            let mut max = contour.max_angle;
            if contour.min_angle - SHADOW_MARGIN > contour.max_angle + SHADOW_MARGIN {
                max += self.spokes.len() as i32;
            }
            for a in contour.min_angle - SHADOW_MARGIN..=max + SHADOW_MARGIN {
                let a = self.mod_spokes(a);
                for r in
                    contour.max_r as usize..=min(4 * contour.max_r as usize, self.spokes.len() - 1)
                {
                    self.spokes[a].sweep[r] =
                        self.spokes[a].sweep[r].intersection(HistoryPixel::BACKUP);
                    // also clear both Doppler bits
                }
            }
        }

        // Draw the contour in the history. This is copied to the output data
        // on the next sweep.
        for p in &contour.contour {
            self.spokes[p.angle as usize].sweep[p.r as usize].insert(HistoryPixel::CONTOUR);
        }
    }
}

impl TargetSetup {
    pub fn polar2pos(&self, pol: &Polar, own_ship: &ExtendedPosition) -> ExtendedPosition {
        // The "own_ship" in the function call can be the position at an earlier time than the current position
        // converts in a radar image angular data r ( 0 - max_spoke_len ) and angle (0 - spokes_per_revolution) to position (lat, lon)
        // based on the own ship position own_ship
        let mut pos: ExtendedPosition = own_ship.clone();
        // should be revised, use Mercator formula PositionBearingDistanceMercator()  TODO
        pos.pos.lat += (pol.r as f64 / self.pixels_per_meter)  // Scale to fraction of distance from radar
                                       * pol.angle_in_rad(self.spokes_per_revolution_f64).cos()
            / METERS_PER_DEGREE_LATITUDE;
        pos.pos.lon += (pol.r as f64 / self.pixels_per_meter)  // Scale to fraction of distance to radar
                                       * pol.angle_in_rad(self.spokes_per_revolution_f64).sin()
            / meters_per_degree_longitude(own_ship.pos.lat);
        pos
    }

    pub fn pos2polar(&self, p: &ExtendedPosition, own_ship: &ExtendedPosition) -> Polar {
        // converts in a radar image a lat-lon position to angular data relative to position own_ship

        let dif_lat = p.pos.lat - own_ship.pos.lat;
        let dif_lon = (p.pos.lon - own_ship.pos.lon) * own_ship.pos.lat.to_radians().cos();
        let r = ((dif_lat * dif_lat + dif_lon * dif_lon).sqrt()
            * METERS_PER_DEGREE_LATITUDE
            * self.pixels_per_meter
            + 1.) as i32;
        let mut angle =
            f64::atan2(dif_lon, dif_lat) * self.spokes_per_revolution_f64 / (2. * PI) + 1.; // + 1 to minimize rounding errors
        if angle < 0. {
            angle += self.spokes_per_revolution_f64;
        }
        return Polar::new(angle as i32, r, p.time);
    }

    pub fn mod_spokes(&self, angle: i32) -> i32 {
        (angle + self.spokes_per_revolution) % self.spokes_per_revolution
    }

    /// Number of sweeps that a next scan of the target may have moved, 1/10th of circle
    pub fn scan_margin(&self) -> i32 {
        self.spokes_per_revolution / 10
    }
}

impl TargetBuffer {
    pub fn new(session: Session, info: &RadarInfo) -> Self {
        let stationary = session.read().unwrap().args.stationary;
        let spokes_per_revolution = info.spokes_per_revolution as i32;
        let spoke_len = info.max_spoke_len as i32;

        TargetBuffer {
            session: session.clone(),
            setup: TargetSetup {
                radar_id: info.id,
                spokes_per_revolution,
                spokes_per_revolution_f64: spokes_per_revolution as f64,
                spoke_len,
                have_doppler: info.doppler,
                pixels_per_meter: 0.0,

                rotation_speed_ms: 0,
                stationary,
            },
            next_target_id: 0,
            arpa_via_doppler: false,

            history: HistorySpokes::new(session.clone(), spokes_per_revolution, spoke_len),
            targets: Arc::new(RwLock::new(HashMap::new())),
            m_clear_contours: false,
            m_auto_learn_state: 0,

            course: 0.,
            course_weight: 0,
            course_samples: 0,

            scanned_angle: -1,
            refreshed_angle: -1,
        }
    }

    pub fn set_rotation_speed(&mut self, ms: u32) {
        self.setup.rotation_speed_ms = ms;
    }

    pub fn set_arpa_via_doppler(&mut self, arpa: bool) -> Result<(), ControlError> {
        if arpa && !self.setup.have_doppler {
            return Err(ControlError::NotSupported("doppler_auto_track".to_string()));
        }
        self.arpa_via_doppler = arpa;
        Ok(())
    }

    fn reset_history(&mut self) {
        self.history = HistorySpokes::new(
            self.session.clone(),
            self.setup.spokes_per_revolution,
            self.setup.spoke_len,
        );
    }

    fn clear_contours(&mut self) {
        for (_, t) in self.targets.write().unwrap().iter_mut() {
            t.contour.length = 0;
            t.m_average_contour_length = 0;
        }
    }
    fn get_next_target_id(&mut self) -> usize {
        const MAX_TARGET_ID: usize = 100000;

        self.next_target_id += 1;
        if self.next_target_id >= MAX_TARGET_ID {
            self.next_target_id = 1;
        }

        self.next_target_id + MAX_TARGET_ID * self.setup.radar_id
    }

    //
    // FUNCTIONS COMING FROM "RadarArpa"
    //

    fn find_target_id_by_position(&self, pos: &GeoPosition) -> Option<usize> {
        let mut best_id = None;
        let mut min_dist = 1000.;
        for (id, target) in self.targets.read().unwrap().iter() {
            if target.m_status != TargetStatus::LOST {
                let dif_lat = pos.lat - target.position.pos.lat;
                let dif_lon = (pos.lon - target.position.pos.lon) * pos.lat.to_radians().cos();
                let dist2 = dif_lat * dif_lat + dif_lon * dif_lon;
                if dist2 < min_dist {
                    min_dist = dist2;
                    best_id = Some(*id);
                }
            }
        }

        best_id
    }

    fn acquire_new_marpa_target(&mut self, target_pos: ExtendedPosition) {
        self.acquire_or_delete_marpa_target(target_pos, TargetStatus::Acquire0);
    }

    /// Delete the target that is closest to the position   
    fn delete_target(&mut self, pos: &GeoPosition) {
        if let Some(id) = self.find_target_id_by_position(pos) {
            self.targets.write().unwrap().remove(&id);
        } else {
            log::debug!(
                "Could not find (M)ARPA target to delete within 1000 meters from {}",
                pos
            );
        }
    }

    fn acquire_or_delete_marpa_target(
        &mut self,
        target_pos: ExtendedPosition,
        status: TargetStatus,
    ) {
        // acquires new target from mouse click position
        // no contour taken yet
        // target status acquire0
        // returns in X metric coordinates of click
        // constructs Kalman filter
        // make new target

        log::debug!("Adding (M)ARPA target at {}", target_pos.pos);

        let id = self.get_next_target_id();
        let target = ArpaTarget::new(
            target_pos,
            GeoPosition::new(0., 0.),
            id,
            self.setup.spokes_per_revolution as usize,
            status,
            self.setup.have_doppler,
        );
        self.targets.write().unwrap().insert(id, target);
    }

    fn cleanup_lost_targets(&mut self) {
        // remove targets with status LOST
        self.targets
            .write()
            .unwrap()
            .retain(|_, t| t.m_status != TargetStatus::LOST);
        for (_, v) in self.targets.write().unwrap().iter_mut() {
            v.m_refreshed = RefreshState::NotFound;
        }
    }

    ///
    /// Refresh all targets between two angles
    ///
    fn refresh_all_arpa_targets(&mut self, start_angle: i32, end_angle: i32) {
        if self.setup.pixels_per_meter == 0. {
            return;
        }
        log::debug!("refresh_all_arpa_targets({}, {})", start_angle, end_angle);
        self.cleanup_lost_targets();

        // main target refresh loop

        // pass 0 of target refresh  Only search for moving targets faster than 2 knots as long as autolearnng is initializing
        // When autolearn is ready, apply for all targets

        let speed = MAX_DETECTION_SPEED_KN * KN_TO_MS; // m/sec
        let search_radius =
            (speed * TODO_ROTATION_SPEED_MS as f64 * self.setup.pixels_per_meter / 1000.) as i32;
        log::debug!(
            "refresh_all_arpa_targets with search radius={}, pix/m={}",
            search_radius,
            self.setup.pixels_per_meter
        );

        for pass in Pass::iter() {
            let radius = match pass {
                Pass::First => search_radius / 4,
                Pass::Second => search_radius / 3,
                Pass::Third => search_radius,
            };

            for (_id, target) in self.targets.write().unwrap().iter_mut() {
                if !target
                    .contour
                    .position
                    .angle_is_between(start_angle, end_angle)
                {
                    continue;
                }
                if pass == Pass::First
                    && !((target.position.speed_kn >= 2.5
                        && target.age_rotations >= TODO_TARGET_AGE_TO_MIXER)
                        || self.m_auto_learn_state >= 1)
                {
                    continue;
                }
                let clone = target.clone();
                match ArpaTarget::refresh_target(
                    clone,
                    &self.setup,
                    &mut self.history,
                    radius / 4,
                    pass,
                ) {
                    Ok(t) => *target = t,
                    Err(e) => {
                        match e {
                            Error::Lost => {
                                // Delete the target
                            }
                            _ => {
                                log::debug!("Target {} refresh error {:?}", target.m_target_id, e);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn delete_all_targets(&mut self) {
        self.targets.write().unwrap().clear();
    }

    /**
     * Inject the target in this radar.
     *
     * Called from the main thread and from the InterRadar receive thread.
     */
    /*
    void RadarArpa::InsertOrUpdateTargetFromOtherRadar(const DynamicTargetData* data, bool remote) {
      wxCriticalSectionLocker lock(m_ri.m_exclusive);

      // This method works on the other radar than TransferTargetToOtherRadar
      // find target
      bool found = false;
      int uid = data->target_id;
      LOG_ARPA(wxT("%s: InsertOrUpdateTarget id=%i"), m_ri.m_name, uid);
      ArpaTarget* updated_target = 0;
      for (auto target = m_targets.begin(); target != m_targets.end(); target++) {
        //LOG_ARPA(wxT("%s: InsertOrUpdateTarget id=%i, found=%i"), m_ri.m_name, uid, (*target).m_target_id);
        if ((*target).m_target_id == uid) {  // target found!
          updated_target = (*target).get();
          found = true;
          LOG_ARPA(wxT("%s: InsertOrUpdateTarget found target id=%d pos=%d"), m_ri.m_name, uid, target - m_targets.begin());
          break;
        }
      }
      if (!found) {
        // make new target with existing uid
        LOG_ARPA(wxT("%s: InsertOrUpdateTarget new target id=%d, pos=%ld"), m_ri.m_name, uid, m_targets.size());
    #ifdef __WXMSW__
        std::unique_ptr<ArpaTarget> new_target = std::make_unique<ArpaTarget>(m_pi, m_ri, uid);
        #else
        std::unique_ptr<ArpaTarget> new_target = make_unique<ArpaTarget>(m_pi, m_ri, uid);
        #endif
        updated_target = new_target.get();
        m_targets.push_back(std::move(new_target));
        ExtendedPosition own_pos;
        if (remote) {
          m_ri->GetRadarPosition(&own_pos);
          Polar pol = updated_target->Pos2Polar(data->position, own_pos);
          LOG_ARPA(wxT("%s: InsertOrUpdateTarget new target id=%d polar=%i"), m_ri.m_name, uid, pol.angle);
          // set estimated time of last refresh as if it was a local target
          updated_target.m_refresh_time = m_ri.m_history[MOD_SPOKES(pol.angle)].time;
        }
      }
      //LOG_ARPA(wxT("%s: InsertOrUpdateTarget processing id=%i"), m_ri.m_name, uid);
      updated_target.m_kalman.P = data->P;
      updated_target.m_position = data->position;
      updated_target.m_status = data->status;
      LOG_ARPA(wxT("%s: transferred id=%i, lat= %f, lon= %f, status=%i,"), m_ri.m_name, updated_target.m_target_id,
               updated_target.m_position.pos.lat, updated_target.m_position.pos.lon, updated_target.m_status);
      updated_target.m_doppler_target = ANY;
      updated_target.m_lost_count = 0;
      updated_target.m_automatic = true;
      double s1 = updated_target.m_position.dlat_dt;  // m per second
      double s2 = updated_target.m_position.dlon_dt;                                   // m  per second
      updated_target.m_course = rad2deg(atan2(s2, s1));
      if (remote) {   // inserted or updated target originated from another radar
        updated_target.m_transferred_target = true;
        //LOG_ARPA(wxT(" m_transferred_target = true targetid=%i"), updated_target.m_target_id);
      }
      return;
    }
    */
    /*

    bool RadarArpa::AcquireNewARPATarget(Polar pol, int status, Doppler doppler) {
      // acquires new target at polar position pol
      // no contour taken yet
      // target status status, normally 0, if dummy target to delete a target -2
      // constructs Kalman filter
      ExtendedPosition own_pos;
      ExtendedPosition target_pos;
      Doppler doppl = doppler;
      if (!m_ri->GetRadarPosition(&own_pos.pos)) {
        return false;
      }

      // make new target
    #ifdef __WXMSW__
      std::unique_ptr<ArpaTarget> target = std::make_unique<ArpaTarget>(m_pi, m_ri, 0);
      #else
      std::unique_ptr<ArpaTarget> target = make_unique<ArpaTarget>(m_pi, m_ri, 0);
      #endif
      target_pos = target->Polar2Pos(pol, own_pos);
      target.m_doppler_target = doppl;
      target.m_position = target_pos;  // Expected position
      target.m_position.time = wxGetUTCTimeMillis();
      target.m_position.dlat_dt = 0.;
      target.m_position.dlon_dt = 0.;
      target.m_position.speed_kn = 0.;
      target.m_position.sd_speed_kn = 0.;
      target.m_status = status;
      target.m_max_angle.angle = 0;
      target.m_min_angle.angle = 0;
      target.m_max_r.r = 0;
      target.m_min_r.r = 0;
      target.m_doppler_target = doppl;
      target.m_refreshed = NOT_FOUND;
      target.m_automatic = true;
      target->RefreshTarget(TARGET_SEARCH_RADIUS1, 1);

      m_targets.push_back(std::move(target));
      return true;
    }

    void RadarArpa::ClearContours() { m_clear_contours = true; }
    */

    /*
    void RadarArpa::ProcessIncomingMessages() {
      DynamicTargetData* target;
      if (m_clear_contours) {
        m_clear_contours = false;
        for (auto target = m_targets.begin(); target != m_targets.end(); target++) {
          (*target).m_contour_length = 0;
          (*target).m_previous_contour_length = 0;
        }
      }

      while ((target = GetIncomingRemoteTarget()) != NULL) {
        InsertOrUpdateTargetFromOtherRadar(target, true);
        delete target;
      }
    }
      */

    /*
    bool RadarArpa::IsAtLeastOneRadarTransmitting() {
      for (size_t r = 0; r < RADARS; r++) {
        if (m_pi.m_radar[r] != NULL && m_pi.m_radar[r].m_state.GetValue() == RADAR_TRANSMIT) {
          return true;
        }
      }
      return false;
    }
      */

    /*
    void RadarArpa::SearchDopplerTargets() {
      ExtendedPosition own_pos;

      if (!m_pi.m_settings.show                       // No radar shown
          || !m_ri->GetRadarPosition(&own_pos.pos)     // No position
          || m_pi->GetHeadingSource() == HEADING_NONE  // No heading
          || (m_pi->GetHeadingSource() == HEADING_FIX_HDM && m_pi.m_var_source == VARIATION_SOURCE_NONE)) {
        return;
      }

      if (m_ri.m_pixels_per_meter == 0. || !IsAtLeastOneRadarTransmitting()) {
        return;
      }

      size_t range_start = 20;  // Convert from meters to 0..511
      size_t range_end;
      int outer_limit = m_ri.m_spoke_len_max;
      outer_limit = (int)outer_limit * 0.93;
      range_end = outer_limit;  // Convert from meters to 0..511

      SpokeBearing start_bearing = 0;
      SpokeBearing end_bearing = m_ri.m_spokes;

      // loop with +2 increments as target must be larger than 2 pixels in width
      for (int angleIter = start_bearing; angleIter < end_bearing; angleIter += 2) {
        SpokeBearing angle = MOD_SPOKES(angleIter);
        wxLongLong angle_time = m_ri.m_history[angle].time;
        // angle_time_plus_margin must be timed later than the pass 2 in refresh, otherwise target may be found multiple times
        wxLongLong angle_time_plus_margin = m_ri.m_history[MOD_SPOKES(angle + 3 * SCAN_MARGIN)].time;

        // check if target has been refreshed since last time
        // and if the beam has passed the target location with SCAN_MARGIN spokes
        if ((angle_time > (m_doppler_arpa_update_time[angle] + SCAN_MARGIN2) &&
             angle_time_plus_margin >= angle_time)) {  // the beam sould have passed our "angle" AND a
                                                       // point SCANMARGIN further set new refresh time
          m_doppler_arpa_update_time[angle] = angle_time;
          for (int rrr = (int)range_start; rrr < (int)range_end; rrr++) {
            if (m_ri.m_arpa->MultiPix(angle, rrr, ANY_DOPPLER)) {
              // pixel found that does not belong to a known target
              Polar pol;
              pol.angle = angle;
              pol.r = rrr;
              if (!m_ri.m_arpa->AcquireNewARPATarget(pol, 0, ANY_DOPPLER)) {
                break;
              }
            }
          }
        }
      }

      return;
    }
    */

    /*
    DynamicTargetData* RadarArpa::GetIncomingRemoteTarget() {
      wxCriticalSectionLocker lock(m_remote_target_lock);
      DynamicTargetData* next;
      if (m_remote_target_queue.empty()) {
        next = NULL;
      } else {
        next = m_remote_target_queue.front();
        m_remote_target_queue.pop_front();
      }
      return next;
    }
    */

    /*
     * Safe to call from any thread
     */
    /*
    void RadarArpa::StoreRemoteTarget(DynamicTargetData* target) {
      wxCriticalSectionLocker lock(m_remote_target_lock);
      m_remote_target_queue.push_back(target);
    }
      */

    fn sample_course(&mut self, bearing: &Option<u32>) {
        let hdt = bearing
            .map(|x| x as f64 / self.setup.spokes_per_revolution_f64)
            .or_else(|| navdata::get_heading_true());

        if let Some(mut hdt) = hdt {
            self.course_samples += 1;
            if self.course_samples == 128 {
                self.course_samples = 0;
                while self.course - hdt > 180. {
                    hdt += 360.;
                }
                while self.course - hdt < -180. {
                    hdt -= 360.;
                }
                if self.course_weight < 16 {
                    self.course_weight += 1;
                }
                self.course += (self.course - hdt) / self.course_weight as f64;
            }
        }
    }

    fn acquire_new_arpa_target(
        &mut self,
        pol: Polar,
        own_pos: GeoPosition,
        time: u64,
        status: TargetStatus,
        doppler: &Doppler,
    ) {
        let epos = ExtendedPosition::new(own_pos, 0., 0., time, 0., 0.);
        let epos = self.setup.polar2pos(&pol, &epos);
        let uid = self.get_next_target_id();

        let target = ArpaTarget::new(
            epos,
            own_pos,
            uid,
            self.setup.spokes_per_revolution as usize,
            status,
            *doppler == Doppler::AnyDoppler,
        );
        //target->RefreshTarget(TARGET_SEARCH_RADIUS1, 1);

        self.targets.write().unwrap().insert(uid, target);
    }

    /// Work on the targets when spoke `angle` has just been processed.
    /// We look for targets a while back, so one quarter rotation ago.
    fn detect_doppler_arpa(&mut self, angle: usize) {
        let end_angle = self.setup.mod_spokes(
            angle as i32 + SCAN_FOR_NEW_PERCENTAGE * self.setup.spokes_per_revolution / 100,
        );

        if self.scanned_angle == -1 {
            self.scanned_angle = self.setup.mod_spokes(
                angle as i32 + REFRESH_END_PERCENTAGE * self.setup.spokes_per_revolution / 100,
            );
        }

        let mut angle = self.scanned_angle;
        loop {
            angle = self.setup.mod_spokes(angle + 2);
            if angle >= end_angle {
                break;
            }

            for r in 20..self.setup.spoke_len - 20 {
                if self.history.multi_pix(&Doppler::AnyDoppler, angle, r) {
                    let time = self.history.spokes[angle as usize].time.clone();
                    let pol = Polar::new(angle, r, time);
                    let own_pos = self.history.spokes[angle as usize].pos.clone();

                    self.acquire_new_arpa_target(
                        pol,
                        own_pos,
                        time,
                        TargetStatus::Acquire0,
                        &Doppler::AnyDoppler,
                    );
                }
            }
        }
        self.scanned_angle = angle;
    }

    /// Work on the targets when spoke `angle` has just been processed.
    /// Refresh older targets from 3 quarters to 2 quarters before what is just received.
    fn refresh_targets(&mut self, angle: usize) {
        let end_angle = self.setup.mod_spokes(
            angle as i32 + REFRESH_END_PERCENTAGE * self.setup.spokes_per_revolution / 100,
        );

        if self.refreshed_angle == -1 {
            self.refreshed_angle = self.setup.mod_spokes(
                angle as i32 + REFRESH_START_PERCENTAGE * self.setup.spokes_per_revolution / 100,
            );
        }

        self.refresh_all_arpa_targets(self.refreshed_angle, end_angle);
        self.refreshed_angle = end_angle;
    }

    pub(crate) fn process_spoke(&mut self, spoke: &mut Spoke, legend: &Legend) {
        if spoke.range == 0 {
            return;
        }

        let pos = if let (Some(lat), Some(lon)) = (spoke.lat, spoke.lon) {
            GeoPosition {
                lat: lat as f64 * 1e-16,
                lon: lon as f64 * 1e-16,
            }
        } else {
            log::trace!("No radar pos, no (M)ARPA possible");
            return;
        };

        let time = spoke.time.unwrap();
        self.sample_course(&spoke.bearing); // Calculate course as the moving average of m_hdt over one revolution

        // TODO main bang size erase
        // TODO: Range Adjustment compensation

        let pixels_per_meter = spoke.data.len() as f64 / spoke.range as f64;

        if self.setup.pixels_per_meter != pixels_per_meter {
            log::debug!(
                " detected spoke range change from {} to {} pixels/m, {} meters",
                self.setup.pixels_per_meter,
                pixels_per_meter,
                spoke.range
            );
            self.setup.pixels_per_meter = pixels_per_meter;
            self.reset_history();
            self.clear_contours();
        }

        // TODO: Think about orientation -- I don't think we have one in mayara: it is always head up?

        let stabilized_mode = spoke.bearing.is_some();
        let weakest_normal_blob = legend.strong_return;
        let angle = if stabilized_mode {
            spoke.bearing.unwrap()
        } else {
            spoke.angle
        } as usize;

        let background_on = self.setup.stationary; // TODO m_autolearning_on_off.GetValue() == 1;
        self.history.spokes[angle].time = time;
        self.history.spokes[angle].sweep.clear();
        self.history.spokes[angle].pos = pos;
        self.history.spokes[angle]
            .sweep
            .resize(spoke.data.len(), HistoryPixel::INITIAL);

        for radius in 0..spoke.data.len() {
            if spoke.data[radius] >= weakest_normal_blob {
                // and add 1 if above threshold and set the left 0 and 1 bits, used for ARPA
                self.history.spokes[angle].sweep[radius] = HistoryPixel::INITIAL; // 1100 0000
                if background_on {
                    if let Some(layer) = self.history.stationary_layer.as_deref_mut() {
                        if layer[[angle, radius]] < u8::MAX {
                            layer[[angle, radius]] += 1;
                        }
                    }
                    // count the number of hits for this pixel
                }
            }

            if spoke.data[radius] == legend.doppler_approaching {
                //  approaching doppler target (255)
                // and add 1 if above threshold and set bit 2, used for ARPA
                self.history.spokes[angle].sweep[radius].insert(HistoryPixel::APPROACHING);
            }

            if spoke.data[radius] == legend.doppler_receding {
                //  receding doppler target (254)
                // and add 1 if above threshold and set bit 3, used for ARPA
                self.history.spokes[angle].sweep[radius].insert(HistoryPixel::RECEDING);
            }

            // Draw the contour
            if self.history.spokes[angle].sweep[radius].contains(HistoryPixel::CONTOUR) {
                spoke.data[radius] = legend.border;
            }
        }

        self.detect_doppler_arpa(angle);
        self.refresh_targets(angle);

        // TODO GUARD ZONES
        // for zone in 0..GUARD_ZONES {
        //if (m_guard_zone[z]->m_alarm_on) {
        //  m_guard_zone[z]->ProcessSpoke(angle, data, m_history[bearing].line, len);
        //}
        //}
    }
}

impl ServerContour {
    fn new() -> ServerContour {
        ServerContour {
            length: 0,
            min_angle: 0,
            max_angle: 0,
            min_r: 0,
            max_r: 0,
            position: Polar::new(0, 0, 0),
            contour: Vec::new(),
        }
    }
}

impl ArpaTarget {
    fn new(
        position: ExtendedPosition,
        radar_pos: GeoPosition,
        uid: usize,
        spokes_per_revolution: usize,
        m_status: TargetStatus,
        have_doppler: bool,
    ) -> Self {
        // makes new target with an existing id
        Self {
            m_status,
            m_average_contour_length: 0,
            m_small_fast: false,
            m_previous_contour_length: 0,
            m_lost_count: 0,
            m_refresh_time: 0,
            m_automatic: false,
            m_radar_pos: radar_pos,
            m_course: 0.,
            m_stationary: 0,
            m_doppler_target: Doppler::Any,
            m_refreshed: RefreshState::NotFound,
            m_target_id: uid,
            m_transferred_target: false,
            m_kalman: KalmanFilter::new(spokes_per_revolution),
            contour: ServerContour::new(),
            m_total_pix: 0,
            m_approaching_pix: 0,
            m_receding_pix: 0,
            have_doppler,
            position,
            expected: Polar::new(0, 0, 0),
            age_rotations: 0,
        }
    }

    fn refresh_target_not_found(
        mut target: ArpaTarget,
        pol: Polar,
        pass: Pass,
    ) -> Result<Self, Error> {
        // target not found
        log::debug!(
            "Not found id={}, angle={}, r={}, pass={:?}, lost_count={}, status={:?}",
            target.m_target_id,
            pol.angle,
            pol.r,
            pass,
            target.m_lost_count,
            target.m_status
        );

        if target.m_small_fast && pass == Pass::Second && target.m_status == TargetStatus::Acquire2
        {
            // status 2, as it was not found,status was not increased.
            // small and fast targets MUST be found in the third sweep, and on a small distance, that is in pass 1.
            log::debug!("smallandfast set lost id={}", target.m_target_id);
            return Err(Error::Lost);
        }

        // delete low status targets immediately when not found
        if ((target.m_status == TargetStatus::Acquire1
            || target.m_status == TargetStatus::Acquire2)
            && pass == Pass::Third)
            || target.m_status == TargetStatus::Acquire0
        {
            log::debug!(
                "low status deleted id={}, angle={}, r={}, pass={:?}, lost_count={}",
                target.m_target_id,
                pol.angle,
                pol.r,
                pass,
                target.m_lost_count
            );
            return Err(Error::Lost);
        }
        if pass == Pass::Third {
            target.m_lost_count += 1;
        }

        // delete if not found too often
        if target.m_lost_count > MAX_LOST_COUNT {
            return Err(Error::Lost);
        }
        target.m_refreshed = RefreshState::NotFound;
        // Send RATTM message also for not seen messages
        /*
        if (pass == LAST_PASS && m_status > m_ri.m_target_age_to_mixer.GetValue()) {
            pol = Pos2Polar(self.position, own_pos);
            if (m_status >= m_ri.m_target_age_to_mixer.GetValue()) {
                //   f64 dist2target = pol.r / self.pixels_per_meter;
                LOG_ARPA(wxT(" pass not found as AIVDM targetid=%i"), m_target_id);
                if (m_transferred_target) {
                    //  LOG_ARPA(wxT(" passTTM targetid=%i"), m_target_id);
                    //  f64 s1 = self.position.dlat_dt;                                   // m per second
                    //  f64 s2 = self.position.dlon_dt;                                   // m  per second
                    //  m_course = rad2deg(atan2(s2, s1));
                    //  PassTTMtoOCPN(&pol, s);

                    PassAIVDMtoOCPN(&pol);
                }
                // MakeAndTransmitTargetMessage();
                // MakeAndTransmitCoT();
            }
        }
        */

        // The target wasn't found, but we do want to keep it around
        // as it may pop up on the next scan.
        target.m_transferred_target = false;
        return Ok(target);
    }

    fn refresh_target(
        mut target: ArpaTarget,
        setup: &TargetSetup,
        history: &mut HistorySpokes,
        dist: i32,
        pass: Pass,
    ) -> Result<Self, Error> {
        // refresh may be called from guard directly, better check
        let own_pos = crate::navdata::get_radar_position();
        if target.m_status == TargetStatus::LOST
            || target.m_refreshed == RefreshState::OutOfScope
            || own_pos.is_none()
        {
            return Err(Error::Lost);
        }
        if target.m_refreshed == RefreshState::Found {
            return Err(Error::AlreadyFound);
        }

        let own_pos = ExtendedPosition::new(own_pos.unwrap(), 0., 0., 0, 0., 0.);

        let mut pol = setup.pos2polar(&target.position, &own_pos);
        let alfa0 = pol.angle;
        let r0 = pol.r;
        let scan_margin = setup.scan_margin();
        let angle_time = history.spokes[setup.mod_spokes(pol.angle + scan_margin) as usize].time;
        // angle_time is the time of a spoke SCAN_MARGIN spokes forward of the target, if that spoke is refreshed we assume that the target has been refreshed

        let mut rotation_period = setup.rotation_speed_ms as u64;
        if rotation_period == 0 {
            rotation_period = 2500; // default value
        }
        if angle_time < target.m_refresh_time + rotation_period - 100 {
            // the 100 is a margin on the rotation period
            // the next image of the target is not yet there

            return Err(Error::WaitForRefresh);
        }

        // set new refresh time
        target.m_refresh_time = history.spokes[pol.angle as usize].time;
        let prev_position = target.position.clone(); // save the previous target position

        // PREDICTION CYCLE

        log::debug!("Begin prediction cycle m_target_id={}, status={:?}, angle={}, r={}, contour={}, pass={:?}, lat={}, lon={}",
               target.m_target_id, target.m_status, pol.angle, pol.r, target.contour.length, pass, target.position.pos.lat, target.position.pos.lon);

        // estimated new target time
        let delta_t = if target.m_refresh_time >= prev_position.time
            && target.m_status != TargetStatus::Acquire0
        {
            (target.m_refresh_time - prev_position.time) as f64 / 1000. // in seconds
        } else {
            0.
        };

        if target.position.pos.lat > 90. || target.position.pos.lat < -90. {
            log::trace!("Target {} has unlikely latitude", target.m_target_id);
            return Err(Error::Lost);
        }

        let mut x_local = LocalPosition::new(
            (target.position.pos.lat - own_pos.pos.lat) * METERS_PER_DEGREE_LATITUDE,
            (target.position.pos.lon - own_pos.pos.lon)
                * meters_per_degree_longitude(own_pos.pos.lat),
            target.position.dlat_dt,
            target.position.dlon_dt,
        );

        target.m_kalman.predict(&mut x_local, delta_t); // x_local is new estimated local position of the target
                                                        // now set the polar to expected angular position from the expected local position

        pol.angle = setup.mod_spokes(
            (f64::atan2(x_local.lon, x_local.lat) * setup.spokes_per_revolution_f64 / (2. * PI))
                as i32,
        );
        pol.r = ((x_local.lat * x_local.lat + x_local.lon * x_local.lon).sqrt()
            * setup.pixels_per_meter) as i32;

        // zooming and target movement may  cause r to be out of bounds
        log::trace!("PREDICTION m_target_id={}, pass={:?}, status={:?}, angle={}.{}, r={}.{}, contour={}, speed={}, sd_speed_kn={} doppler={:?}, lostcount={}",
               target.m_target_id, pass, target.m_status, alfa0, pol.angle, r0, pol.r, target.contour.length, target.position.speed_kn,
               target.position.sd_speed_kn, target.m_doppler_target, target.m_lost_count);
        if pol.r >= setup.spoke_len || pol.r <= 0 {
            // delete target if too far out
            log::trace!(
                "R out of bounds,  m_target_id={}, angle={}, r={}, contour={}, pass={:?}",
                target.m_target_id,
                pol.angle,
                pol.r,
                target.contour.length,
                pass
            );
            return Err(Error::Lost);
        }
        target.expected = pol; // save expected polar position

        // MEASUREMENT CYCLE
        // now search for the target at the expected polar position in pol
        let mut dist1 = dist;

        if pass == Pass::Third {
            // this is doubtfull $$$
            if target.m_status == TargetStatus::Acquire0
                || target.m_status == TargetStatus::Acquire1
            {
                dist1 *= 2;
            } else if target.position.speed_kn > 15. {
                dist1 *= 2;
            } /*else if (self.position.speed_kn > 30.) {
                dist1 *= 4;
              } */
        }

        let starting_position = pol;

        // here we really search for the target
        if pass == Pass::Third {
            target.m_doppler_target = Doppler::Any; // in the last pass we are not critical
        }
        let found = history.get_target(&target.m_doppler_target, pol.clone(), dist1); // main target search

        match found {
            Ok((contour, pos)) => {
                let dist_angle =
                    ((pol.angle - starting_position.angle) as f64 * pol.r as f64 / 326.) as i32;
                let dist_radial = pol.r - starting_position.r;
                let dist_total =
                    ((dist_angle * dist_angle + dist_radial * dist_radial) as f64).sqrt() as i32;

                log::debug!("id={}, Found dist_angle={}, dist_radial={}, dist_total={}, pol.angle={}, starting_position.angle={}, doppler={:?}", 
        target.m_target_id,
                 dist_angle, dist_radial, dist_total, pol.angle, starting_position.angle, target.m_doppler_target);

                if target.m_doppler_target != Doppler::Any {
                    let backup = target.m_doppler_target;
                    target.m_doppler_target = Doppler::Any;
                    let _ = history.get_target(&target.m_doppler_target, pol.clone(), dist1); // get the contour for the target ins ANY state
                    target.pixel_counter(history);
                    target.m_doppler_target = backup;
                    let _ = history.get_target(&target.m_doppler_target, pol.clone(), dist1); // restore target in original state
                    target.state_transition(); // adapt state if required
                } else {
                    target.pixel_counter(history);
                    target.state_transition();
                }
                if target.m_average_contour_length != 0
                    && (target.contour.length < target.m_average_contour_length / 2
                        || target.contour.length > target.m_average_contour_length * 2)
                    && pass != Pass::Third
                {
                    return Err(Error::WeightedContourLengthTooHigh);
                }

                history.reset_pixels(&contour, &pos, &setup.pixels_per_meter);
                log::debug!("target Found ResetPixels m_target_id={}, angle={}, r={}, contour={}, pass={:?}, doppler={:?}",
                 target.m_target_id, pol.angle, pol.r, target.contour.length, pass, target.m_doppler_target);
                if target.contour.length >= MAX_CONTOUR_LENGTH as i32 - 2 {
                    // don't use this blob, could be radar interference
                    // The pixels of the blob have been reset, so you won't find it again
                    log::debug!("reset found because of max contour length id={}, angle={}, r={}, contour={}, pass={:?}", 
                target.m_target_id, pol.angle,
                 pol.r, target.contour.length, pass);
                    return Err(Error::ContourTooLong);
                }

                target.m_lost_count = 0;
                let mut p_own = ExtendedPosition::empty();
                p_own.pos = history.spokes[history.mod_spokes(pol.angle) as usize].pos;
                target.age_rotations += 1;
                target.m_status = match target.m_status {
                    TargetStatus::Acquire0 => TargetStatus::Acquire1,
                    TargetStatus::Acquire1 => TargetStatus::Acquire2,
                    TargetStatus::Acquire2 => TargetStatus::ACQUIRE3,
                    TargetStatus::ACQUIRE3 | TargetStatus::ACTIVE => TargetStatus::ACTIVE,
                    _ => TargetStatus::Acquire0,
                };
                if target.m_status == TargetStatus::Acquire0 {
                    // as this is the first measurement, move target to measured position
                    // ExtendedPosition p_own;
                    // p_own.pos = m_ri.m_history[MOD_SPOKES(pol.angle)].pos;  // get the position at receive time
                    target.position = setup.polar2pos(&pol, &mut p_own); // using own ship location from the time of reception, only lat and lon
                    target.position.dlat_dt = 0.;
                    target.position.dlon_dt = 0.;
                    target.position.sd_speed_kn = 0.;
                    target.expected = pol;
                    log::debug!(
                        "calculated id={} pos={}",
                        target.m_target_id,
                        target.position.pos
                    );
                    target.age_rotations = 0;
                }

                // Kalman filter to  calculate the apostriori local position and speed based on found position (pol)
                if target.m_status == TargetStatus::Acquire2
                    || target.m_status == TargetStatus::ACQUIRE3
                {
                    target.m_kalman.update_covariance();
                    target.m_kalman.update(
                        &pol,
                        &mut x_local,
                        &target.expected,
                        setup.pixels_per_meter,
                    ); // pol is measured position in polar coordinates
                }
                // x_local expected position in local coordinates

                target.position.time = pol.time; // set the target time to the newly found time, this is the time the spoke was received

                if target.m_status != TargetStatus::Acquire1 {
                    // if status == 1, then this was first measurement, keep position at measured position
                    target.position.pos.lat =
                        own_pos.pos.lat + x_local.lat / METERS_PER_DEGREE_LATITUDE;
                    target.position.pos.lon = own_pos.pos.lon
                        + x_local.lon / meters_per_degree_longitude(own_pos.pos.lat);
                    target.position.dlat_dt = x_local.dlat_dt; // meters / sec
                    target.position.dlon_dt = x_local.dlon_dt; // meters /sec
                    target.position.sd_speed_kn = x_local.sd_speed * MS_TO_KN;
                }

                // Here we bypass the Kalman filter to predict the speed of the target
                // Kalman filter is too slow to adjust to the speed of (fast) new targets
                // This method however only works for targets where the accuricy of the position is high,
                // that is small targets in relation to the size of the target.

                if target.m_status == TargetStatus::Acquire2 {
                    // determine if this is a small and fast target
                    let dist_angle = pol.angle - alfa0;
                    let dist_r = pol.r - r0;
                    let size_angle = max(
                        history.mod_spokes(target.contour.max_angle - target.contour.min_angle),
                        1,
                    );
                    let size_r = max(target.contour.max_r - target.contour.min_r, 1);
                    let test = (dist_r as f64 / size_r as f64).abs()
                        + (dist_angle as f64 / size_angle as f64).abs();
                    target.m_small_fast = test > 2.;
                    log::debug!("smallandfast, id={}, test={}, dist_r={}, size_r={}, dist_angle={}, size_angle={}",
          target.m_target_id, test, dist_r, size_r, dist_angle, size_angle);
                }

                const FORCED_POSITION_STATUS: u32 = 8;
                const FORCED_POSITION_AGE_FAST: u32 = 5;

                if target.m_small_fast
                    && target.age_rotations >= 2
                    && target.age_rotations < FORCED_POSITION_STATUS
                    && (target.age_rotations < FORCED_POSITION_AGE_FAST
                        || target.position.speed_kn > 10.)
                {
                    // Do a linear extrapolation of the estimated position instead of the kalman filter, as it
                    // takes too long to get up to speed for these targets.
                    let prev_pos = prev_position.pos;
                    let new_pos = setup.polar2pos(&pol, &p_own).pos;
                    let delta_lat = new_pos.lat - prev_pos.lat;
                    let delta_lon = new_pos.lon - prev_pos.lon;
                    let delta_t = pol.time - prev_position.time;
                    if delta_t > 1000 {
                        // delta_t < 1000; speed unreliable due to uncertainties in location
                        let d_lat_dt =
                            (delta_lat / (delta_t as f64)) * METERS_PER_DEGREE_LATITUDE * 1000.;
                        let d_lon_dt = (delta_lon / (delta_t as f64))
                            * meters_per_degree_longitude(new_pos.lat)
                            * 1000.;
                        log::debug!("id={}, FORCED m_status={:?}, d_lat_dt={}, d_lon_dt={}, delta_lon_meter={}, delta_lat_meter={}, deltat={}",
                   target.m_target_id, target.m_status, d_lat_dt, d_lon_dt,
                     delta_lon * METERS_PER_DEGREE_LATITUDE, delta_lat * METERS_PER_DEGREE_LATITUDE, delta_t);
                        // force new position and speed, dependent of overridefactor

                        let factor: f64 = (0.8_f64).powf((target.age_rotations - 1) as f64);
                        target.position.pos.lat += factor * (new_pos.lat - target.position.pos.lat);
                        target.position.pos.lon += factor * (new_pos.lon - target.position.pos.lon);
                        target.position.dlat_dt += factor * (d_lat_dt - target.position.dlat_dt); // in meters/sec
                        target.position.dlon_dt += factor * (d_lon_dt - target.position.dlon_dt);
                        // in meters/sec
                    }
                }

                // set refresh time to the time of the spoke where the target was found
                target.m_refresh_time = target.position.time;
                if target.age_rotations >= 1 {
                    let s1 = target.position.dlat_dt; // m per second
                    let s2 = target.position.dlon_dt; // m  per second
                    target.position.speed_kn = (s1 * s1 + s2 * s2).sqrt() * MS_TO_KN; // and convert to nautical miles per hour
                    target.m_course = f64::atan2(s2, s1).to_degrees();
                    if target.m_course < 0. {
                        target.m_course += 360.;
                    }

                    log::debug!("FOUND {:?} CYCLE id={}, status={:?}, age={}, angle={}.{}, r={}.{}, contour={}, speed={}, sd_speed_kn={}, doppler={:?}",
                        pass, target.m_target_id, target.m_status, target.age_rotations, alfa0, pol.angle, r0, pol.r, target.contour.length, target.position.speed_kn,
                        target.position.sd_speed_kn, target.m_doppler_target);

                    target.m_previous_contour_length = target.contour.length;
                    // send target data to OCPN and other radar

                    const WEIGHT_FACTOR: f64 = 0.1;

                    if target.contour.length != 0 {
                        if target.m_average_contour_length == 0 && target.contour.length != 0 {
                            target.m_average_contour_length = target.contour.length;
                        } else {
                            target.m_average_contour_length +=
                                ((target.contour.length - target.m_average_contour_length) as f64
                                    * WEIGHT_FACTOR) as i32;
                        }
                    }

                    //if (m_status >= m_ri.m_target_age_to_mixer.GetValue()) {
                    //  f64 dist2target = pol.r / self.pixels_per_meter;
                    // TODO: PassAIVDMtoOCPN(&pol);  // status s not yet used

                    // TODO: MakeAndTransmitTargetMessage();

                    // MakeAndTransmitCoT();
                    //}

                    target.m_refreshed = RefreshState::Found;
                    // A target that has been found is no longer considered a transferred target
                    target.m_transferred_target = false;
                }
            }
            Err(_e) => return Self::refresh_target_not_found(target, pol, pass),
        };
        return Ok(target);
    }

    /// Count the number of pixels in the target, and the number of approaching and receding pixels
    ///
    /// It works by moving outwards from all borders of the target until there is no target pixel
    /// at that radius. On the outside of the target this should only count 1, but on the inside
    /// it will count all pixels in the sweep until it hits the outside. The number of pixels
    /// is not fully correct: the outside pixels are counted twice.
    ///
    fn pixel_counter(&mut self, history: &HistorySpokes) {
        //  Counts total number of the various pixels in a blob
        self.m_total_pix = 0;
        self.m_approaching_pix = 0;
        self.m_receding_pix = 0;
        for i in 0..self.contour.contour.len() {
            for radius in 0..history.spokes[0].sweep.len() {
                let pixel =
                    history.spokes[history.mod_spokes(self.contour.contour[i].angle)].sweep[radius];
                let target = pixel.contains(HistoryPixel::TARGET); // above threshold bit
                if !target {
                    break;
                }
                let approaching = pixel.contains(HistoryPixel::APPROACHING); // this is Doppler approaching bit
                let receding = pixel.contains(HistoryPixel::RECEDING); // this is Doppler receding bit
                self.m_total_pix += target as u32;
                self.m_approaching_pix += approaching as u32;
                self.m_receding_pix += receding as u32;
            }
        }
    }

    /// Check doppler state of targets if Doppler is on
    fn state_transition(&mut self) {
        if !self.have_doppler || self.m_doppler_target == Doppler::AnyPlus {
            return;
        }

        let check_to_doppler = (self.m_total_pix as f64 * 0.85) as u32;
        let check_not_approaching =
            ((self.m_total_pix - self.m_approaching_pix) as f64 * 0.80) as u32;
        let check_not_receding = ((self.m_total_pix - self.m_receding_pix) as f64 * 0.80) as u32;

        let new = match self.m_doppler_target {
            Doppler::AnyDoppler | Doppler::Any => {
                // convert to APPROACHING or RECEDING
                if self.m_approaching_pix > self.m_receding_pix
                    && self.m_approaching_pix > check_to_doppler
                {
                    &Doppler::Approaching
                } else if self.m_receding_pix > self.m_approaching_pix
                    && self.m_receding_pix > check_to_doppler
                {
                    &Doppler::Receding
                } else if self.m_doppler_target == Doppler::AnyDoppler {
                    &Doppler::Any
                } else {
                    &self.m_doppler_target
                }
            }

            Doppler::Receding => {
                if self.m_receding_pix < check_not_approaching {
                    &Doppler::Any
                } else {
                    &self.m_doppler_target
                }
            }

            Doppler::Approaching => {
                if self.m_approaching_pix < check_not_receding {
                    &Doppler::Any
                } else {
                    &self.m_doppler_target
                }
            }
            _ => &self.m_doppler_target,
        };
        if *new != self.m_doppler_target {
            log::debug!(
                "Target {} Doppler state changed from {:?} to {:?}",
                self.m_target_id,
                self.m_doppler_target,
                new
            );
            self.m_doppler_target = *new;
        }
    }

    /*
        void ArpaTarget::TransferTargetToOtherRadar() {
          RadarInfo* other_radar = 0;
          LOG_ARPA(wxT("%s: TransferTargetToOtherRadar m_target_id=%i,"), m_ri.m_name, m_target_id);
          if (M_SETTINGS.radar_count != 2) {
            return;
          }
          if (!m_pi.m_radar[0] || !m_pi.m_radar[1] || !m_pi.m_radar[0].m_arpa || !m_pi.m_radar[1].m_arpa) {
            return;
          }
          if (m_pi.m_radar[0].m_state.GetValue() != RADAR_TRANSMIT || m_pi.m_radar[1].m_state.GetValue() != RADAR_TRANSMIT) {
            return;
          }
          LOG_ARPA(wxT("%s: this  radar pix/m=%f"), m_ri.m_name, self.pixels_per_meter);
          RadarInfo* long_range = m_pi.GetLongRangeRadar();
          RadarInfo* short_range = m_pi.GetShortRangeRadar();

          if (m_ri == long_range) {
            other_radar = short_range;
            int border = (int)(m_ri.m_spoke_len_max * self.pixels_per_meter / short_range.m_pixels_per_meter);
            // m_ri has largest range, other_radar smaller range. Don't transfer targets that are outside range of smaller radar
            if (m_expected.r > border) {
              // don't send small range targets to smaller radar
              return;
            }
          } else {
            other_radar = long_range;
            // this (m_ri) is the small range radar
            // we will only send larger range targets to other radar
          }
          DynamicTargetData data;
          data.target_id = m_target_id;
          data.P = m_kalman.P;
          data.position = self.position;
          LOG_ARPA(wxT("%s: lat= %f, lon= %f, m_target_id=%i,"), m_ri.m_name, self.position.pos.lat, self.position.pos.lon, m_target_id);
          data.status = m_status;
          other_radar.m_arpa.InsertOrUpdateTargetFromOtherRadar(&data, false);
        }

        void ArpaTarget::SendTargetToNearbyRadar() {
          LOG_ARPA(wxT("%s: Send target to nearby radar, m_target_id=%i,"), m_ri.m_name, m_target_id);
          RadarInfo* long_radar = m_pi.GetLongRangeRadar();
          if (m_ri != long_radar) {
            return;
          }
          DynamicTargetData data;
          data.target_id = m_target_id;
          data.P = m_kalman.P;
          data.position = self.position;
          LOG_ARPA(wxT("%s: lat= %f, lon= %f, m_target_id=%i,"), m_ri.m_name, self.position.pos.lat, self.position.pos.lon, m_target_id);
          data.status = m_status;
          if (m_pi.m_inter_radar) {
            m_pi.m_inter_radar.SendTarget(data);
            LOG_ARPA(wxT(" %s, target data send id=%i"), m_ri.m_name, m_target_id);
          }
        }

    */

    /*
    void ArpaTarget::PassAIVDMtoOCPN(Polar* pol) {
      if (!m_ri.m_AIVDMtoO.GetValue()) return;
      wxString s_TargID, s_Bear_Unit, s_Course_Unit;
      wxString s_speed, s_course, s_Dist_Unit, s_status;
      wxString s_bearing;
      wxString s_distance;
      wxString s_target_name;
      wxString nmea;

      if (m_status == LOST) return;  // AIS has no "status lost" message
      s_Bear_Unit = wxEmptyString;   // Bearing Units  R or empty
      s_Course_Unit = wxT("T");      // Course type R; Realtive T; true
      s_Dist_Unit = wxT("N");        // Speed/Distance Unit K, N, S N= NM/h = Knots

      // f64 dist = pol.r / self.pixels_per_meter / NAUTICAL_MILE.;
      f64 bearing = pol.angle * 360. / m_ri.m_spokes;
      if (bearing < 0) bearing += 360;

      int mmsi = m_target_id % 1000000;
      GeoPosition radar_pos;
      m_ri.GetRadarPosition(&radar_pos);
      f64 target_lat, target_lon;

      target_lat = self.position.pos.lat;
      target_lon = self.position.pos.lon;
      wxString result = EncodeAIVDM(mmsi, self.position.speed_kn, target_lon, target_lat, m_course);
      PushNMEABuffer(result);
      m_pi.SendToTargetMixer(result);
    }

    void ArpaTarget::PassTTMtoOCPN(Polar* pol, OCPN_target_status status) {
      // if (!m_ri.m_TTMtoO.GetValue()) return;  // also remove from conf file
      wxString s_TargID, s_Bear_Unit, s_Course_Unit;
      wxString s_speed, s_course, s_Dist_Unit, s_status;
      wxString s_bearing;
      wxString s_distance;
      wxString s_target_name;
      wxString nmea;
      char sentence[90];
      char checksum = 0;
      char* p;
      s_Bear_Unit = wxEmptyString;  // Bearing Units  R or empty
      s_Course_Unit = wxT("T");     // Course type R; Realtive T; true
      s_Dist_Unit = wxT("N");       // Speed/Distance Unit K, N, S N= NM/h = Knots
      // switch (status) {
      // case Q:
      //   s_status = wxT("Q");  // yellow
      //   break;
      // case T:
      //   s_status = wxT("T");  // green
      //   break;
      // case L:
      //   LOG_ARPA(wxT(" id=%i, status == lost"), m_target_id);
      //   s_status = wxT("L");  // ?
      //   break;
      // }

      if (m_doppler_target == ANY) {
        s_status = wxT("Q");  // yellow
      } else {
        s_status = wxT("T");
      }

      f64 dist = pol.r / self.pixels_per_meter / NAUTICAL_MILE.;
      f64 bearing = pol.angle * 360. / m_ri.m_spokes;

      if (bearing < 0) bearing += 360;
      s_TargID = wxString::Format(wxT("%2i"), m_target_id);
      s_speed = wxString::Format(wxT("%4.2f"), self.position.speed_kn);
      s_course = wxString::Format(wxT("%3.1f"), m_course);
      if (m_automatic) {
        s_target_name = wxString::Format(wxT("ARPA%2i"), m_target_id);
      } else {
        s_target_name = wxString::Format(wxT("MARPA%2i"), m_target_id);
      }
      s_distance = wxString::Format(wxT("%f"), dist);
      s_bearing = wxString::Format(wxT("%f"), bearing);

      /* Code for TTM follows. Send speed and course using TTM*/
      snprintf(sentence, sizeof(sentence), "RATTM,%2s,%s,%s,%s,%s,%s,%s, , ,%s,%s,%s, ",
               (const char*)s_TargID.mb_str(),       // 1 target id
               (const char*)s_distance.mb_str(),     // 2 Targ distance
               (const char*)s_bearing.mb_str(),      // 3 Bearing fr own ship.
               (const char*)s_Bear_Unit.mb_str(),    // 4 Brearing unit ( T = true)
               (const char*)s_speed.mb_str(),        // 5 Target speed
               (const char*)s_course.mb_str(),       // 6 Target Course.
               (const char*)s_Course_Unit.mb_str(),  // 7 Course ref T // 8 CPA Not used // 9 TCPA Not used
               (const char*)s_Dist_Unit.mb_str(),    // 10 S/D Unit N = knots/Nm
               (const char*)s_target_name.mb_str(),  // 11 Target name
               (const char*)s_status.mb_str());      // 12 Target Status L/Q/T // 13 Ref N/A

      for (p = sentence; *p; p++) {
        checksum ^= *p;
      }
      nmea.Printf(wxT("$%s*%02X\r\n"), sentence, (unsigned)checksum);
      LOG_ARPA(wxT("%s: send TTM, target=%i string=%s"), m_ri.m_name, m_target_id, nmea);
      PushNMEABuffer(nmea);
    }

    #define COPYTOMESSAGE(xxx, bitsize)                   \
      for (int i = 0; i < bitsize; i++) {                 \
        bitmessage[index - i - 1] = xxx[bitsize - i - 1]; \
      }                                                   \
      index -= bitsize;

    wxString ArpaTarget::EncodeAIVDM(int mmsi, f64 speed, f64 lon, f64 lat, f64 course) {
      // For encoding !AIVDM type 1 messages following the spec in https://gpsd.gitlab.io/gpsd/AIVDM.html
      // Sender is ecnoded as AI. There is no official identification for radar targets.

      bitset<168> bitmessage;
      int index = 168;
      bitset<6> type(1);  // 6
      COPYTOMESSAGE(type, 6);
      bitset<2> repeat(0);  // 8
      COPYTOMESSAGE(repeat, 2);
      bitset<30> mmsix(mmsi);  // 38
      COPYTOMESSAGE(mmsix, 30);
      bitset<4> navstatus(0);  // under way using engine    // 42
      COPYTOMESSAGE(navstatus, 4);
      bitset<8> rot(0);  // not turning                  // 50
      COPYTOMESSAGE(rot, 8);
      bitset<10> speedx(round(speed * 10));  // 60
      COPYTOMESSAGE(speedx, 10);
      bitset<1> accuracy(0);  // 61
      COPYTOMESSAGE(accuracy, 1);
      bitset<28> lonx(round(lon * 600000));  // 89
      COPYTOMESSAGE(lonx, 28);
      bitset<27> latx(round(lat * 600000));  // 116
      COPYTOMESSAGE(latx, 27);
      bitset<12> coursex(round(course * 10));  // COG       // 128
      COPYTOMESSAGE(coursex, 12);
      bitset<9> true_heading(511);  // 137
      COPYTOMESSAGE(true_heading, 9);
      bitset<6> timestamp(60);  // 60 means not available   // 143
      COPYTOMESSAGE(timestamp, 6);
      bitset<2> maneuvre(0);  // 145
      COPYTOMESSAGE(maneuvre, 2);
      bitset<3> spare;  // 148
      COPYTOMESSAGE(spare, 3);
      bitset<1> flags(0);  // 149
      COPYTOMESSAGE(flags, 1);
      bitset<19> rstatus(0);  // 168
      COPYTOMESSAGE(rstatus, 19);
      wxString AIVDM = "AIVDM,1,1,,A,";
      bitset<6> char_data;
      uint8_t character;
      for (int i = 168; i > 0; i -= 6) {
        for (int j = 0; j < 6; j++) {
          char_data[j] = bitmessage[i - 6 + j];
        }
        character = (uint8_t)char_data.to_ulong();
        if (character > 39) character += 8;
        character += 48;
        AIVDM += character;
      }
      AIVDM += ",0";
      // calculate checksum
      char checks = 0;
      for (size_t i = 0; i < AIVDM.length(); i++) {
        checks ^= (char)AIVDM[i];
      }
      AIVDM.Printf(wxT("!%s*%02X\r\n"), AIVDM, (unsigned)checks);
      LOG_ARPA(wxT("%s: AIS length=%i, string=%s"), m_ri.m_name, AIVDM.length(), AIVDM);
      return AIVDM;
    }

    void ArpaTarget::MakeAndTransmitCoT() {  // currently not used, CoT messages for WinTak are made bij the Targetmixer
      int mmsi = m_target_id;
      wxString mmsi_string;                                    // uid="MMSI - 001000001"
      mmsi_string.Printf(wxT(" uid=\"RADAR - %09i\""), mmsi);  // uid="MMSI - 001000002"
      wxString short_mmsi_string;
      short_mmsi_string.Printf(wxT("\"%09i\""), mmsi);
      wxDateTime dt(self.position.time);
      int year = dt.GetYear();
      int month = dt.GetMonth();
      int day = dt.GetDay();
      int hour = dt.GetHour();
      int minute = dt.GetMinute();
      int second = dt.GetSecond();
      int millisecond = dt.GetMillisecond();
      wxString speed_string;
      speed_string.Printf(wxT("\"%f\""), self.position.speed_kn);
      wxString course_string;
      course_string.Printf(wxT("\"%f\""), m_course);
    #define LIFE 4

      wxString date_time_string;        // "2022-11-12T09:29:34.784
      wxString date_time_string_stale;  // "2022-11-12T09:29:34.784
      date_time_string.Printf(wxT("\"%04i-%02i-%02iT%02i:%02i:%02i.%03i"), year, month, day, hour, minute, second, millisecond);
      date_time_string_stale.Printf(wxT("\"%04i-%02i-%02iT%02i:%02i:%02i.%03i"), year, month, day, hour, minute, second + LIFE,
                                    millisecond);
      wxString long_date_time_string;  //  time="2022-11-12T09:29:34.784000Z" start="2022-11-12T09:29:34.784000Z"
                                       //  stale="2022-11-12T09:29:34.784000Z"
      // start must be later than time, stale later than start
      long_date_time_string =
          " time=" + date_time_string + "000Z\" start=" + date_time_string + "100Z\" stale=" + date_time_string_stale + "000Z\"";
      wxString position_string;  // lat="53.461441" lon="6.178049"
      position_string.Printf(wxT(" lat=\"%f\" lon=\"%f\""), self.position.pos.lat, self.position.pos.lon);
      wxString version_string = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\" ?>";

      wxString CoTxml;
      CoTxml = version_string;
      CoTxml += "<event version=\"2.0\" type=\"a-u-S\"" + mmsi_string + " how=\"m-g\"" + long_date_time_string + ">";
      CoTxml += "<point" + position_string + " hae=\"9.0\" le=\"9.0\" ce=\"9.0\" />";
      CoTxml += "<detail" + mmsi_string + ">";
      CoTxml += "<track course=" + course_string + " speed=" + speed_string + " />";
      // CoTxml += "<contact callsign=" + short_mmsi_string + " />";
      // Remarks is not required
      // CoTxml += "<remarks>Country: Netherlands(Kingdom of the) Type : 1 MMSI : 244730001 aiscot@kees-m14.verruijt.lan</remarks>";
      CoTxml += "</detail>";
      // < _aiscot_ is not required
      // CoTxml += "<_aiscot_ cot_host_id = \"aiscot@kees-m14.verruijt.lan\" country=\"Netherlands (Kingdom of the)\" type=\"1\"
      // mmsi=\"244730002\" aton=\"False\" uscg=\"False\" crs=\"False\" />";
      CoTxml += "</event>";
      LOG_ARPA(wxT("%s: COTxml=\n%s"), m_ri.m_name, CoTxml);
      m_pi.SendToTargetMixer(CoTxml);
    }

    void ArpaTarget::MakeAndTransmitTargetMessage() {
      /* Example message
      {"target":{"uid":1,"lat":52.038339,"lon":4.111908,"sog": 2.08,"cog":39.3,
      "time":"2022-12-29T15:38:11.307000Z","stale":"2022-12-29T15:38:15.307000Z","state":5,
      "lost_count":0}}

      */

    #define LIFE_TIME_SEC 3 // life time of target after last refresh in seconds

  wxString message = wxT("{\"target\":{");

  message << wxT("\"uid\":");
  message << m_target_id;

  message << wxT(",\"source_id\":");
  message << m_pi.m_radar_id;

  message << wxT(",\"lat\":");
  message << self.position.pos.lat;
  message << wxT(",\"lon\":");
  message << self.position.pos.lon;

  wxDateTime dt(self.position.time);
  dt = dt.ToUTC();
  message << wxT(",\"time\":\"");
  message << dt.FormatISOCombined() << wxString::Format(wxT(".%03uZ\""), dt.GetMillisecond(wxDateTime::TZ::GMT0));

  dt += wxTimeSpan(0, 0, LIFE_TIME_SEC, 0);
  message << wxT(",\"stale\":\"");
  message << dt.FormatISOCombined() << wxString::Format(wxT(".%03uZ\""), dt.GetMillisecond(wxDateTime::TZ::GMT0));

  message << wxString::Format(wxT(",\"sog\":%5.2f"), self.position.speed_kn * NAUTICAL_MILE. / 3600.);
  message << wxString::Format(wxT(",\"cog\":%4.1f"), m_course);

  message << wxT(",\"state\":");
  message << m_status;
  message << wxT(",\"lost_count\":");
  message << m_lost_count;

  message << wxT("}}");

  m_pi.SendToTargetMixer(message);
}
  */

    fn set_status_lost(&mut self) {
        self.contour = ServerContour::new();
        self.m_previous_contour_length = 0;
        self.m_lost_count = 0;
        self.m_kalman.reset();
        self.m_status = TargetStatus::LOST;
        self.m_automatic = false;
        self.m_refresh_time = 0;
        self.m_course = 0.;
        self.m_stationary = 0;
        self.position.dlat_dt = 0.;
        self.position.dlon_dt = 0.;
        self.position.speed_kn = 0.;
    }
}
