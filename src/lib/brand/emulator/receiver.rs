//! What the emulated receiver makes of the returns: the clutter the sea
//! around the boat throws back, the sea control that suppresses it close
//! in, and the gain that lifts or sinks every echo and, turned up, the
//! receiver's own noise.

use super::coast_echo::lattice;
use super::world::Echo;

/// The strongest echo a pixel can show
const MAX_INTENSITY: f64 = 15.0;
/// A return weaker than this does not stand out from the background, and
/// the radar shows nothing there
const DETECTION: f64 = 1.5;
/// Gain and sea are set from 0 to 100
const FULL_SCALE: f64 = 100.0;
/// Where gain changes nothing
const NEUTRAL_GAIN: f64 = 50.0;
/// How many steps of intensity full gain adds, and no gain takes away:
/// turned down, even a ship fades
const GAIN_LIFT: f64 = 6.0;
const GAIN_CUT: f64 = 12.0;
/// Above this gain the receiver's own noise starts to speckle the picture,
/// unless sea control holds it down
const NOISE_FROM_GAIN: f64 = 65.0;
/// The share of pixels noise lights at full gain without sea control: as on
/// a Furuno DRS4D-NXT at gain 100 with sea and rain off, nearly all of them
const NOISE_SHARE_AT_FULL_GAIN: f64 = 0.97;
/// Sea control at this setting or above, or on auto, clears the noise; a
/// real radar at gain 100 with auto sea shows hardly any beyond a mile
const SEA_CLEARING_NOISE: f64 = 50.0;
/// How strong noise is on average before gain lifts it; it varies from
/// pixel to pixel, so the noise shows mottled over the whole scale
const NOISE_INTENSITY: f64 = 5.0;
/// Noise comes in blotches about this many spokes wide and pixels deep,
/// not pixel by pixel
const NOISE_BLOTCH_SPOKES: u16 = 6;
const NOISE_BLOTCH_PIXELS: usize = 3;

/// How strong the sea's return is right by the boat, in a moderate sea
const SEA_CLUTTER_AT_BOAT: f64 = 7.0;
/// How far out, in meters, sea clutter falls to about a third
const SEA_CLUTTER_RANGE: f64 = 500.0;
/// How much full sea control takes off close in; as clutter, it fades
/// with range, so far targets keep their strength
const SEA_CONTROL_AT_BOAT: f64 = 24.0;
/// What sea control auto settles on: enough to clear most of the clutter
const AUTO_SEA: f64 = 45.0;

/// Gain and sea travel as 0 to 255 on the wire, and are set as 0 to 100
const PERCENT_TO_WIRE: f64 = 2.55;

const CLUTTER_SEED: u64 = 0xc1a7_0001;
const NOISE_SEED: u64 = 0xc1a7_0002;
const NOISE_LEVEL_SEED: u64 = 0xc1a7_0003;

/// The gain and sea settings, as the radar's controls report them
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Receiver {
    pub gain: f64,
    pub gain_auto: bool,
    pub sea: f64,
    pub sea_auto: bool,
    /// How much the sea around the boat returns, 0 for a flat calm
    sea_state: f64,
}

impl Default for Receiver {
    fn default() -> Self {
        Receiver {
            gain: NEUTRAL_GAIN,
            gain_auto: true,
            sea: AUTO_SEA,
            sea_auto: true,
            sea_state: 1.0,
        }
    }
}

impl Receiver {
    /// A receiver on a flat calm, which adds nothing to what it is given
    #[cfg(test)]
    pub(crate) fn calm() -> Self {
        Receiver {
            sea_state: 0.0,
            ..Receiver::default()
        }
    }

    /// A gain update: a value or auto flag the update leaves out stays as it was
    pub(crate) fn set_gain(&mut self, value: Option<f64>, auto: Option<bool>) {
        self.gain = value.unwrap_or(self.gain);
        self.gain_auto = auto.unwrap_or(self.gain_auto);
    }

    /// A sea update: a value or auto flag the update leaves out stays as it was
    pub(crate) fn set_sea(&mut self, value: Option<f64>, auto: Option<bool>) {
        self.sea = value.unwrap_or(self.sea);
        self.sea_auto = auto.unwrap_or(self.sea_auto);
    }

    /// Gain as the radar's controls report it: the wire value and the auto flag
    pub(crate) fn gain_on_wire(&self) -> (f64, u8) {
        (self.gain * PERCENT_TO_WIRE, u8::from(self.gain_auto))
    }

    /// Sea as the radar's controls report it: the wire value and the auto flag
    pub(crate) fn sea_on_wire(&self) -> (f64, u8) {
        (self.sea * PERCENT_TO_WIRE, u8::from(self.sea_auto))
    }

    fn gain_level(&self) -> f64 {
        if self.gain_auto {
            NEUTRAL_GAIN
        } else {
            self.gain
        }
    }

    fn sea_level(&self) -> f64 {
        if self.sea_auto { AUTO_SEA } else { self.sea }
    }

    /// The share of empty pixels the receiver's own noise lights
    fn noise_share(&self) -> f64 {
        let gain =
            ((self.gain_level() - NOISE_FROM_GAIN) / (FULL_SCALE - NOISE_FROM_GAIN)).max(0.0);
        let unsuppressed = if self.sea_auto {
            0.0
        } else {
            (1.0 - self.sea / SEA_CLEARING_NOISE).max(0.0)
        };
        gain * unsuppressed * NOISE_SHARE_AT_FULL_GAIN
    }

    /// The echo shown at pixel `pixel` of spoke `spoke`, `distance` meters
    /// out, in rotation `rotation`, for what came back there.
    pub(crate) fn pixel(
        &self,
        echo: Echo,
        distance: f64,
        spoke: u16,
        pixel: usize,
        rotation: u32,
    ) -> Echo {
        let close_in = (-distance / SEA_CLUTTER_RANGE).exp();
        let speckle = |seed: u64| lattice(spoke as i64, pixel as i64, seed ^ rotation as u64);

        // Sea clutter flickers from sweep to sweep: an exponential speckle
        // around its mean, as the waves move
        let clutter =
            self.sea_state * SEA_CLUTTER_AT_BOAT * close_in * -(1.0 - speckle(CLUTTER_SEED)).ln();
        let (mut strength, mut closing_speed) = if clutter > echo.intensity as f64 {
            (clutter, 0.0)
        } else {
            (echo.intensity as f64, echo.closing_speed)
        };
        if strength > 0.0 {
            // In whole steps, so sea control leaves land and ships further
            // out at the strength they would show without it
            strength -= (self.sea_level() / FULL_SCALE * SEA_CONTROL_AT_BOAT * close_in).floor();
        }
        // Clutter too weak to stand out stays in the background, which gain
        // brings up only as the receiver's noise
        if echo.intensity == 0 && strength < DETECTION {
            strength = 0.0;
        }

        let gain = self.gain_level();
        if strength <= 0.0 && speckle(NOISE_SEED) < self.noise_share() {
            let blotch = lattice(
                (spoke / NOISE_BLOTCH_SPOKES) as i64,
                (pixel / NOISE_BLOTCH_PIXELS) as i64,
                NOISE_LEVEL_SEED ^ rotation as u64,
            );
            strength = NOISE_INTENSITY * -(1.0 - blotch).ln();
            // Noise does not move, whatever echo sea control cleared here
            closing_speed = 0.0;
        }
        if strength <= 0.0 {
            return Echo {
                intensity: 0,
                closing_speed: 0.0,
            };
        }

        let shift = if gain >= NEUTRAL_GAIN {
            (gain - NEUTRAL_GAIN) / (FULL_SCALE - NEUTRAL_GAIN) * GAIN_LIFT
        } else {
            (gain - NEUTRAL_GAIN) / NEUTRAL_GAIN * GAIN_CUT
        };
        let lifted = strength + shift;
        if lifted < DETECTION {
            return Echo {
                intensity: 0,
                closing_speed: 0.0,
            };
        }
        let shown = lifted.round().min(MAX_INTENSITY) as u8;
        Echo {
            intensity: shown,
            closing_speed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPOKES: u16 = 512;
    const PIXELS: usize = 200;
    /// Meters per pixel in the test pictures
    const STEP: f64 = 10.0;

    fn echo(intensity: u8) -> Echo {
        Echo {
            intensity,
            closing_speed: 0.0,
        }
    }

    /// How many pixels of an empty sea light up, close in and further out,
    /// over one rotation
    fn lit(receiver: &Receiver, rotation: u32) -> (usize, usize) {
        let mut near = 0;
        let mut far = 0;
        for spoke in 0..SPOKES {
            for pixel in 0..PIXELS {
                let distance = pixel as f64 * STEP;
                if receiver
                    .pixel(echo(0), distance, spoke, pixel, rotation)
                    .intensity
                    > 0
                {
                    if distance < 500.0 {
                        near += 1;
                    } else if distance >= 1500.0 {
                        far += 1;
                    }
                }
            }
        }
        (near, far)
    }

    fn manual(gain: f64, sea: f64) -> Receiver {
        Receiver {
            gain,
            gain_auto: false,
            sea,
            sea_auto: false,
            ..Receiver::default()
        }
    }

    /// On a calm sea at the neutral gain without sea control every echo
    /// shows as it came back.
    #[test]
    fn a_calm_neutral_receiver_passes_echoes_unchanged() {
        let calm = Receiver {
            sea_state: 0.0,
            ..manual(NEUTRAL_GAIN, 0.0)
        };
        for intensity in 2..=15 {
            let moving = Echo {
                intensity,
                closing_speed: 3.0,
            };
            assert_eq!(calm.pixel(moving, 800.0, 7, 80, 1), moving);
        }
    }

    /// Without sea control the sea lights up close to the boat and hardly
    /// at all further out; sea control clears it close in.
    #[test]
    fn sea_clutter_lies_close_in_and_sea_control_clears_it() {
        let (near, far) = lit(&manual(NEUTRAL_GAIN, 0.0), 1);
        assert!(near > 1000, "clutter close in: {near}");
        assert!(far < near / 20, "little far out: {far} vs {near}");

        let (cleared, _) = lit(&manual(NEUTRAL_GAIN, 100.0), 1);
        assert!(cleared < near / 10, "sea control: {cleared} vs {near}");

        let (auto, _) = lit(&Receiver::default(), 1);
        assert!(auto < near / 3, "auto sea clears most: {auto} vs {near}");
    }

    /// The clutter changes from one sweep to the next, as waves do.
    #[test]
    fn sea_clutter_flickers_between_rotations() {
        let receiver = manual(NEUTRAL_GAIN, 0.0);
        let changed = (0..PIXELS)
            .filter(|&pixel| {
                let at = |rotation| {
                    receiver
                        .pixel(echo(0), pixel as f64 * 2.0, 3, pixel, rotation)
                        .intensity
                };
                at(1) != at(2)
            })
            .count();
        assert!(changed > PIXELS / 4, "{changed}");
    }

    /// Too much sea control also takes away a small boat close in, as on a
    /// real radar; far out it is untouched.
    #[test]
    fn full_sea_control_hides_a_weak_echo_close_in_only() {
        let receiver = Receiver {
            sea_state: 0.0,
            ..manual(NEUTRAL_GAIN, 100.0)
        };
        assert_eq!(receiver.pixel(echo(6), 50.0, 0, 5, 0).intensity, 0);
        assert_eq!(receiver.pixel(echo(6), 5000.0, 0, 500, 0).intensity, 6);
    }

    /// Auto sea leaves land a nautical mile out at full strength.
    #[test]
    fn auto_sea_leaves_land_a_mile_out_as_it_is() {
        let receiver = Receiver::calm();
        assert_eq!(receiver.pixel(echo(14), 1852.0, 0, 185, 0).intensity, 14);
        assert!(receiver.pixel(echo(14), 100.0, 0, 10, 0).intensity < 14);
    }

    /// Gain lifts and sinks every echo: turned right down even a ship
    /// barely shows, turned up a weak beacon shows strongly.
    #[test]
    fn gain_lifts_and_sinks_echoes() {
        let at = |gain: f64, intensity| {
            Receiver {
                sea_state: 0.0,
                ..manual(gain, 0.0)
            }
            .pixel(echo(intensity), 3000.0, 0, 300, 0)
            .intensity
        };
        assert_eq!(at(NEUTRAL_GAIN, 6), 6);
        assert!(at(100.0, 6) > 6);
        assert_eq!(at(100.0, 14), 15);
        assert_eq!(at(0.0, 6), 0, "a beacon vanishes");
        assert!(at(0.0, 15) <= 3, "a ship barely shows");
        assert!(at(25.0, 11) < 11 && at(25.0, 11) > 0);
    }

    /// Turned up past the noise, gain speckles empty water; below it the
    /// water stays dark.
    #[test]
    fn high_gain_brings_up_noise() {
        let quiet = Receiver {
            sea_state: 0.0,
            ..manual(NOISE_FROM_GAIN, 0.0)
        };
        let loud = Receiver {
            sea_state: 0.0,
            ..manual(FULL_SCALE, 0.0)
        };
        let (quiet_near, quiet_far) = lit(&quiet, 1);
        let (loud_near, loud_far) = lit(&loud, 1);
        assert_eq!(quiet_near + quiet_far, 0);
        assert!(loud_far > 100, "{loud_far}");
        assert!(loud_near > 50, "{loud_near}");
    }

    fn far_pixels() -> usize {
        SPOKES as usize * (PIXELS - (1500.0 / STEP) as usize)
    }

    /// With auto sea, full gain leaves the water far out nearly dark, as a
    /// real radar's auto clutter control does: faint clutter is not lifted
    /// into an even sheet.
    #[test]
    fn full_gain_with_auto_sea_leaves_the_far_sea_dark() {
        let receiver = Receiver {
            sea_auto: true,
            ..manual(FULL_SCALE, 0.0)
        };
        let (_, far) = lit(&receiver, 1);
        assert!(far < far_pixels() / 100, "{far} of {}", far_pixels());
    }

    /// With sea control off, full gain fills the screen with noise of every
    /// strength, mottled rather than one flat colour; sea control clears it.
    #[test]
    fn full_gain_without_sea_control_fills_the_screen() {
        let open = manual(FULL_SCALE, 0.0);
        let (_, far) = lit(&open, 1);
        assert!(far > far_pixels() * 9 / 10, "{far} of {}", far_pixels());

        let mut strengths = std::collections::BTreeSet::new();
        for spoke in 0..SPOKES {
            strengths.insert(open.pixel(echo(0), 1800.0, spoke, 180, 1).intensity);
        }
        assert!(strengths.len() >= 6, "{strengths:?}");

        let (_, cleared) = lit(&manual(FULL_SCALE, SEA_CLEARING_NOISE), 1);
        assert!(cleared < far_pixels() / 100, "{cleared}");
    }

    /// A faint echo is lifted by gain like any other.
    #[test]
    fn gain_lifts_the_faintest_echo() {
        let receiver = Receiver {
            sea_state: 0.0,
            ..manual(FULL_SCALE, 0.0)
        };
        assert_eq!(receiver.pixel(echo(1), 3000.0, 0, 300, 0).intensity, 7);
    }

    /// Where sea control clears a moving ship close in and noise shows in
    /// its place, the noise carries no Doppler of the ship's.
    #[test]
    fn noise_where_sea_control_cleared_a_ship_does_not_move() {
        let receiver = Receiver {
            sea_state: 0.0,
            ..manual(FULL_SCALE, 40.0)
        };
        let ship = Echo {
            intensity: 2,
            closing_speed: 3.0,
        };
        let shown: Vec<Echo> = (0..SPOKES)
            .map(|spoke| receiver.pixel(ship, 0.0, spoke, 0, 1))
            .filter(|e| e.intensity > 0)
            .collect();
        assert!(!shown.is_empty(), "noise shows");
        assert!(shown.iter().all(|e| e.closing_speed == 0.0));
    }

    /// An update sets what it carries and leaves the rest; the controls
    /// report the setting on the radar's 0 to 255 scale with its auto flag.
    #[test]
    fn updates_keep_what_they_leave_out_and_report_on_the_wire_scale() {
        let wire = |(value, auto): (f64, u8)| (value.round(), auto);
        let mut receiver = Receiver::default();
        receiver.set_gain(Some(100.0), Some(false));
        receiver.set_gain(None, None);
        assert_eq!(wire(receiver.gain_on_wire()), (255.0, 0));
        receiver.set_gain(None, Some(true));
        assert_eq!(wire(receiver.gain_on_wire()), (255.0, 1));

        receiver.set_sea(Some(20.0), None);
        assert_eq!(wire(receiver.sea_on_wire()), (51.0, 1));
        receiver.set_sea(None, Some(false));
        assert_eq!(wire(receiver.sea_on_wire()), (51.0, 0));
    }

    /// Auto gain is the neutral gain whatever the manual value was.
    #[test]
    fn auto_gain_ignores_the_manual_value() {
        let auto = Receiver {
            gain: 0.0,
            gain_auto: true,
            ..Receiver::calm()
        };
        assert_eq!(auto.pixel(echo(9), 3000.0, 0, 300, 0).intensity, 9);
    }
}
