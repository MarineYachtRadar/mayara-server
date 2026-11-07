//! How land returns on the emulator's picture: not the chart's clean line
//! and even fill, but a ragged edge, patches of strong and weak return
//! with gaps between them, fading away from the shore, and smeared sideways
//! by the beam as a real radar paints it.

use std::f64::consts::TAU;

use crate::radar::GeoPosition;

const METERS_PER_DEGREE_LAT: f64 = 111_320.0;

/// How far the painted shore wanders either side of the chart's, in meters
const COAST_FRAY: f64 = 150.0;
/// The size of the patches land returns in, in meters: dunes, dykes,
/// buildings, trees
const LAND_PATCH: f64 = 90.0;
/// A patch is never smaller than this many pixels, or land far out would
/// break up into single-pixel speckle
const MIN_PATCH_PIXELS: f64 = 3.0;
/// How deep into the land the beam reaches at the least and at the most,
/// as a share of the land's full depth
const DEPTH_SPREAD: (f64, f64) = (0.8, 2.0);
/// Land at the end of its depth returns this share of what its shore does
const FAR_SHARE: f64 = 0.8;
/// The face of the shore that looks at the radar returns this much more
const SHORE_BOOST: f64 = 0.4;
/// How much the return of a patch changes from one rotation to the next
const FLICKER: f64 = 0.12;
/// Below this return a patch shows nothing
const HOLE: f64 = 0.3;
/// A patch this strong or stronger shows with land's strongest return
const SOLID: f64 = 0.8;
/// How far apart patches lie in strength; value noise alone bunches
/// around the middle
const CONTRAST: f64 = 2.2;
/// The weakest return a patch shows with
const WEAKEST: u8 = 3;

const EDGE_SEED: u64 = 0x5eed_0001;
const DEPTH_SEED: u64 = 0x5eed_0002;
const PATCH_SEED: u64 = 0x5eed_0003;
const DETAIL_SEED: u64 = 0x5eed_0004;
const FLICKER_SEED: u64 = 0x5eed_0005;

/// The land's return for one rotation of the beam. Its patterns hang on
/// the ground, not on the boat, so the coast keeps its shape as the boat
/// sails past it.
pub(crate) struct CoastEcho {
    /// Own ship's longitude and latitude
    boat: (f64, f64),
    meters_per_degree_lon: f64,
    patch: f64,
    strongest: u8,
    depth: f64,
    rotation: u64,
    /// Sine and cosine of the angles off the spoke the beam still sees,
    /// out to half its width either way
    beam_offsets: [(f64, f64); 4],
}

impl CoastEcho {
    /// `depth` is how far into the land the beam reaches, in meters,
    /// `strongest` the return of the brightest patch and `half_beam` half
    /// the beam's width in radians.
    pub(crate) fn new(
        boat: &GeoPosition,
        meters_per_pixel: f64,
        depth: f64,
        strongest: u8,
        half_beam: f64,
        rotation: u32,
    ) -> Self {
        let meters_per_degree_lon = METERS_PER_DEGREE_LAT * boat.lat().to_radians().cos();
        CoastEcho {
            boat: (boat.lon(), boat.lat()),
            meters_per_degree_lon,
            patch: LAND_PATCH.max(meters_per_pixel * MIN_PATCH_PIXELS),
            strongest,
            depth,
            rotation: rotation as u64,
            beam_offsets: [-1.0, -0.5, 0.5, 1.0].map(|share| (share * half_beam).sin_cos()),
        }
    }

    /// The return at `distance` meters along a spoke at bearing (`sin_b`,
    /// `cos_b`) whose stretches over land are `land`; 0 off land and in its
    /// gaps.
    pub(crate) fn at(&self, sin_b: f64, cos_b: f64, distance: f64, land: &[(f64, f64)]) -> u8 {
        let Some(&(enter, leave)) = land
            .iter()
            .find(|&&(enter, leave)| distance >= enter - COAST_FRAY && distance < leave)
        else {
            return 0;
        };
        let (x, y) = self.ground(sin_b, cos_b, distance);

        // Thin land, a sandbank or a dyke, never frays away entirely
        let fray = ((noise(x, y, 2.0 * self.patch, EDGE_SEED) - 0.5) * 2.0 * COAST_FRAY)
            .min((leave - enter) / 2.0);
        let shore = enter + fray;
        let reach = self.depth
            * lerp(
                DEPTH_SPREAD.0,
                DEPTH_SPREAD.1,
                noise(x, y, 4.0 * self.patch, DEPTH_SEED),
            );
        let into = distance - shore;
        if into < 0.0 || into >= reach {
            return 0;
        }

        // The beam returns the strongest patch anywhere across its width,
        // so patches far out stretch sideways as on a real radar
        let texture = self
            .beam_offsets
            .iter()
            .map(|&(sin_d, cos_d)| {
                self.ground(
                    sin_b * cos_d + cos_b * sin_d,
                    cos_b * cos_d - sin_b * sin_d,
                    distance,
                )
            })
            .chain(std::iter::once((x, y)))
            .map(|(x, y)| self.texture(x, y))
            .fold(0.0, f64::max);
        let shore_face = SHORE_BOOST * (1.0 - into / self.patch).max(0.0);
        let fade = 1.0 - into / reach * (1.0 - FAR_SHARE);
        let strength = (texture + shore_face) * fade;
        if strength < HOLE {
            return 0;
        }
        let share = ((strength - HOLE) / (SOLID - HOLE)).min(1.0);
        WEAKEST + (share * (self.strongest - WEAKEST) as f64).round() as u8
    }

    /// Where on the ground a point along a spoke lies, in meters east and
    /// north, so the patterns stay put wherever the boat goes
    fn ground(&self, sin_b: f64, cos_b: f64, distance: f64) -> (f64, f64) {
        let lat = self.boat.1 + distance * cos_b / METERS_PER_DEGREE_LAT;
        let lon = self.boat.0 + distance * sin_b / self.meters_per_degree_lon;
        (
            lon * METERS_PER_DEGREE_LAT * lat.to_radians().cos(),
            lat * METERS_PER_DEGREE_LAT,
        )
    }

    /// How strongly the ground at (`x`, `y`) returns this rotation
    fn texture(&self, x: f64, y: f64) -> f64 {
        // Each finer grid turned against the coarser one, so their squares
        // do not line up into a visible weave
        let (dx, dy) = (0.8 * x - 0.6 * y, 0.6 * x + 0.8 * y);
        let (fx, fy) = (0.6 * x + 0.8 * y, 0.8 * y - 0.6 * x);
        let patches = 0.65 * noise(x, y, self.patch, PATCH_SEED)
            + 0.35 * noise(dx, dy, self.patch / 2.0, DETAIL_SEED);
        let flicker = noise(fx, fy, self.patch / 2.0, FLICKER_SEED ^ self.rotation) - 0.5;
        0.5 + CONTRAST * (patches - 0.5) + FLICKER * flicker
    }
}

/// Spreads each spoke's land over the spokes within `half_beam` radians to
/// either side, as the beam's width paints land beside where it lies.
pub(crate) fn widen_by_beam(spokes: &[Vec<(f64, f64)>], half_beam: f64) -> Vec<Vec<(f64, f64)>> {
    let count = spokes.len();
    let reach = (half_beam / TAU * count as f64).round() as usize;
    (0..count)
        .map(|spoke| {
            let mut stretches: Vec<(f64, f64)> = (0..=2 * reach)
                .flat_map(|i| &spokes[(spoke + count + i - reach) % count])
                .copied()
                .collect();
            stretches.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f64, f64)> = Vec::with_capacity(stretches.len());
            for (enter, leave) in stretches {
                match merged.last_mut() {
                    Some(last) if enter <= last.1 => last.1 = last.1.max(leave),
                    _ => merged.push((enter, leave)),
                }
            }
            merged
        })
        .collect()
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Smooth value noise in [0, 1) over a grid of `cell` meters: the same at
/// the same place, every time.
fn noise(x: f64, y: f64, cell: f64, seed: u64) -> f64 {
    let (gx, gy) = (x / cell, y / cell);
    let (fx, fy) = (gx.floor(), gy.floor());
    let smooth = |t: f64| t * t * (3.0 - 2.0 * t);
    let (tx, ty) = (smooth(gx - fx), smooth(gy - fy));
    let (ix, iy) = (fx as i64, fy as i64);
    let corner = |dx: i64, dy: i64| lattice(ix + dx, iy + dy, seed);
    lerp(
        lerp(corner(0, 0), corner(1, 0), tx),
        lerp(corner(0, 1), corner(1, 1), tx),
        ty,
    )
}

/// A value in [0, 1) for each grid point, from a 64-bit mix of its
/// coordinates
fn lattice(ix: i64, iy: i64, seed: u64) -> f64 {
    let mut h = (ix as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (iy as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
        ^ seed.wrapping_mul(0x1656_67b1_9e37_79f9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 33;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPTH: f64 = 400.0;
    const HALF_BEAM: f64 = 0.0175;
    const STRONGEST: u8 = 14;
    const BOAT: (f64, f64) = (53.2, 5.3);

    fn coast(rotation: u32) -> CoastEcho {
        CoastEcho::new(
            &GeoPosition::new(BOAT.0, BOAT.1),
            5.0,
            DEPTH,
            STRONGEST,
            HALF_BEAM,
            rotation,
        )
    }

    /// Returns along `spokes` spokes fanned out across a turn, every 5 m
    /// from 0 to 3 km, over land from 1 to 2 km on each.
    fn picture(echo: &CoastEcho, spokes: usize) -> Vec<Vec<u8>> {
        let land = [(1000.0, 2000.0)];
        (0..spokes)
            .map(|i| {
                let (sin_b, cos_b) = (i as f64 / spokes as f64 * TAU).sin_cos();
                (0..600)
                    .map(|p| echo.at(sin_b, cos_b, p as f64 * 5.0, &land))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn noise_stays_in_range_and_repeats() {
        for i in 0..1000 {
            let (x, y) = (i as f64 * 37.3, i as f64 * -91.7);
            let n = noise(x, y, 60.0, 1);
            assert!((0.0..1.0).contains(&n));
            assert_eq!(n, noise(x, y, 60.0, 1));
        }
    }

    /// The shore does not follow the chart's line: where the beam first
    /// meets land varies from spoke to spoke, within the fray either side.
    #[test]
    fn the_shore_is_ragged() {
        let first_land: Vec<f64> = picture(&coast(0), 360)
            .iter()
            .map(|spoke| spoke.iter().position(|&v| v > 0).unwrap() as f64 * 5.0)
            .collect();
        let nearest = first_land.iter().copied().fold(f64::INFINITY, f64::min);
        let farthest = first_land.iter().copied().fold(0.0, f64::max);

        assert!(nearest >= 1000.0 - COAST_FRAY, "{nearest}");
        assert!(farthest - nearest > COAST_FRAY, "{nearest}..{farthest}");
    }

    /// Land returns in patches of differing strength with gaps, its
    /// strongest no stronger than land may be, nothing on the water before
    /// the fray and nothing past its depth.
    #[test]
    fn land_returns_in_patches_and_fades_inland() {
        let picture = picture(&coast(0), 360);
        let all: Vec<u8> = picture.iter().flatten().copied().collect();

        assert!(all.iter().all(|&v| v <= STRONGEST));
        assert!(all.contains(&STRONGEST));
        let distinct: std::collections::BTreeSet<u8> = all.iter().copied().collect();
        assert!(distinct.len() > 5, "{distinct:?}");

        let water = ((1000.0 - COAST_FRAY) / 5.0) as usize;
        let past = ((1000.0 + COAST_FRAY + DEPTH * DEPTH_SPREAD.1) / 5.0) as usize + 1;
        assert!(picture.iter().all(|s| s[..water].iter().all(|&v| v == 0)));
        assert!(picture.iter().all(|s| s[past..].iter().all(|&v| v == 0)));

        let land = |range: std::ops::Range<usize>| {
            picture
                .iter()
                .map(|s| s[range.clone()].iter().filter(|&&v| v > 0).count())
                .sum::<usize>()
        };
        let (near, far) = (land(200..240), land(260..300));
        assert!(near > far, "near shore {near}, further in {far}");
        assert!(near < picture.len() * 40, "land has gaps");
    }

    /// The coast is the same seen again, so it stands still on the screen,
    /// and changes only a little from one rotation to the next.
    #[test]
    fn the_coast_holds_its_shape_between_rotations() {
        assert_eq!(picture(&coast(1), 90), picture(&coast(1), 90));

        let (a, b) = (picture(&coast(1), 90), picture(&coast(2), 90));
        let (mut differ, mut land) = (0, 0);
        for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
            land += usize::from(*x > 0 || *y > 0);
            differ += usize::from(x.abs_diff(*y) > 2 || (*x > 0) != (*y > 0));
        }
        assert!(differ * 5 < land, "{differ} of {land} pixels changed");
    }

    /// Land a sailing boat passes keeps where it lies: a point on shore
    /// returns the same seen from two places.
    #[test]
    fn the_pattern_hangs_on_the_ground() {
        let here = GeoPosition::new(BOAT.0, BOAT.1);
        let there = here.position_from_bearing(0.0, 500.0);
        let from = |boat: &GeoPosition| CoastEcho::new(boat, 5.0, DEPTH, STRONGEST, 0.0, 0);
        // A point 1100 m north of `here` is 600 m north of `there`
        let land_here = [(1000.0, 2000.0)];
        let land_there = [(500.0, 1500.0)];
        let mut same = 0;
        for d in 0..40 {
            let d = d as f64 * 5.0;
            let a = from(&here).at(0.0, 1.0, 1000.0 + d, &land_here);
            let b = from(&there).at(0.0, 1.0, 500.0 + d, &land_there);
            same += usize::from(a == b);
        }
        assert!(same >= 38, "{same} of 40");
    }

    /// Land shows on the spokes to either side of where it lies, as far as
    /// half the beam, and keeps its nearest edge.
    #[test]
    fn the_beam_widens_land_to_either_side() {
        let mut spokes = vec![Vec::new(); 360];
        spokes[100] = vec![(1000.0, 1200.0)];
        spokes[101] = vec![(900.0, 1100.0), (3000.0, 3100.0)];

        let wide = widen_by_beam(&spokes, 2.0_f64.to_radians());

        assert!(wide[97].is_empty());
        assert_eq!(wide[98], vec![(1000.0, 1200.0)]);
        assert_eq!(wide[100], vec![(900.0, 1200.0), (3000.0, 3100.0)]);
        assert_eq!(wide[103], vec![(900.0, 1100.0), (3000.0, 3100.0)]);
        assert!(wide[104].is_empty());
    }
}
