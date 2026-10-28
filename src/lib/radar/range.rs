//
// Every radar supports a number of ranges, which are
// the distances at which the radar can detect objects.
//
// Some radars use the index into a well-defined list of ranges,
// while others use a numeric value that represents the range in meters.
// Navico, Raymarine and Garmin radars use a numeric value,
// while Furuno uses an index into a list of ranges.

use std::fmt::{Display, Formatter, Result as FmtResult};
use std::sync::LazyLock;

use crate::radar::{KILOYARD_F64, NM_F64, STATUTE_MILE_F64};

use super::{FRAC_NM_2, FRAC_NM_4, FRAC_NM_8, FRAC_NM_16, FRAC_NM_32, NM};

// What follows are ranges that we try when range detection is attempted
// where the code will send all those ranges to the radar to see if it
// "sticks", and if so it adds it to the supported ranges for that radar.

// All ranges seen on all radars
static ALL_POSSIBLE_NAUTICAL_RANGES: LazyLock<Ranges> = LazyLock::new(|| {
    Ranges::new(vec![
        Range::initial(FRAC_NM_32), // 1/32 nm
        Range::initial(FRAC_NM_16), // 1/16 nm
        Range::initial(FRAC_NM_8),  // 1/8 nm
        Range::initial(FRAC_NM_4),  // 1/4 nm
        Range::initial(FRAC_NM_2),  // 1/2 nm
        Range::initial(NM * 3 / 4), // 3/4 nm
        Range::initial(NM),         // 1 nm
        Range::initial(NM * 3 / 2), // 1.5 nm
        Range::initial(NM * 2),     // 2 nm
        Range::initial(NM * 3),     // 3 nm
        Range::initial(NM * 4),     // 4 nm
        Range::initial(NM * 6),     // 6 nm
        Range::initial(NM * 8),     // 8 nm
        Range::initial(NM * 12),    // 12 nm
        Range::initial(NM * 16),    // 16 nm
        Range::initial(NM * 24),    // 24 nm
        Range::initial(NM * 32),    // 32 nm
        Range::initial(NM * 36),    // 36 nm
        Range::initial(NM * 40),    // 40 nm
        Range::initial(NM * 48),    // 48 nm
        Range::initial(NM * 64),    // 64 nm
        Range::initial(NM * 72),    // 72 nm
        Range::initial(NM * 96),    // 96 nm
        Range::initial(NM * 120),   // 120 nm
    ])
});

// All ranges seen on all radars
static ALL_POSSIBLE_METRIC_RANGES: LazyLock<Ranges> = LazyLock::new(|| {
    Ranges::new(vec![
        Range::initial(50),
        Range::initial(75),
        Range::initial(100),
        Range::initial(250),
        Range::initial(500),
        Range::initial(750),
        Range::initial(1000),
        Range::initial(1500),
        Range::initial(2000),
        Range::initial(3000),
        Range::initial(4000),
        Range::initial(6000),
        Range::initial(8000),
        Range::initial(12000),
        Range::initial(16000),
        Range::initial(24000),
        Range::initial(36000),
        Range::initial(48000),
        Range::initial(64000),
        Range::initial(72000),
        Range::initial(96000),
        Range::initial(120000),
    ])
});

/// The unit a range is shown in. Metres alone tell metric from nautical
/// ranges, but not statute miles or kiloyards from either, so those are
/// named by whoever builds the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    MetricOrNautical,
    StatuteMiles,
    Kiloyards,
}

/// A single radar range entry. `distance` is the only field that
/// participates in equality and ordering; `index` is purely informational
/// (it tags the entry's position in some originating table) and two
/// `Range`s with the same `distance` are considered equal regardless of
/// `index` or `unit`.
#[derive(Debug, Clone, Copy, Eq)]
pub struct Range {
    distance: i32,
    index: usize,
    unit: Unit,
}

impl Range {
    fn initial(value: i32) -> Self {
        Self::new(value, 0)
    }

    pub fn new(value: i32, index: usize) -> Self {
        Self {
            distance: value,
            index,
            unit: Unit::MetricOrNautical,
        }
    }

    pub fn distance(&self) -> i32 {
        self.distance
    }

    pub fn index(&self) -> usize {
        self.index
    }

    fn near(a: i32, b: i32) -> bool {
        a % b == 0 || a % b == 1 || a % b == b - 1
    }

    /// Check if a distance value (in meters) looks like a metric (km-based) range
    pub fn is_metric_distance(v: i32) -> bool {
        Self::metric(v)
    }

    fn metric(v: i32) -> bool {
        // 125m is the "0.125 km" first entry in every Furuno km-mode range
        // table, but the near-multiple-of-50 heuristic below misses it
        // (125 % 50 == 25). Without this special case it pollutes the
        // nautical range list on all Furuno models, landing between
        // 116m (1/16 NM) and 231m (1/8 NM) as a dead zoom step: the
        // closest-match wire-index lookup then maps 125 back to 116,
        // leaving the radar stuck at its minimum range.
        if v == 125 {
            return true;
        }
        if v <= 100 {
            Self::near(v, 25)
        } else if v <= 750 {
            Self::near(v, 50)
        } else {
            Self::near(v, 500)
        }
    }

    pub fn is_metric(&self) -> bool {
        Self::metric(self.distance)
    }

    pub fn is_nautical(&self) -> bool {
        !Self::metric(self.distance)
    }
}

impl Ord for Range {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.distance.cmp(&other.distance)
    }
}
impl PartialOrd for Range {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Range {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

/// Statute mile ranges go down to 1/16 mi, as nautical ones do.
const STATUTE_MILE_FRACTIONS: i32 = 16;

/// Kiloyard ranges are shown to the yard.
const YARDS_PER_KILOYARD: f64 = 1000.;

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Display for Range {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self.unit {
            Unit::StatuteMiles => {
                let miles = f64::from(self.distance) / STATUTE_MILE_F64;
                let sixteenths = (miles * f64::from(STATUTE_MILE_FRACTIONS)).round() as i32;
                if sixteenths < STATUTE_MILE_FRACTIONS {
                    let divisor = gcd(sixteenths, STATUTE_MILE_FRACTIONS);
                    return write!(
                        f,
                        "{}/{} mi",
                        sixteenths / divisor,
                        STATUTE_MILE_FRACTIONS / divisor
                    );
                }
                let miles = f64::from(sixteenths) / f64::from(STATUTE_MILE_FRACTIONS);
                return write!(f, "{} mi", miles);
            }
            Unit::Kiloyards => {
                let kiloyards = f64::from(self.distance) / KILOYARD_F64;
                let kiloyards = (kiloyards * YARDS_PER_KILOYARD).round() / YARDS_PER_KILOYARD;
                return write!(f, "{} kyd", kiloyards);
            }
            Unit::MetricOrNautical => {}
        }
        {
            let v = self.distance;
            if Range::metric(v) {
                // Metric
                if v >= 1000 {
                    if v % 1000 == 0 {
                        write!(f, "{} km", v / 1000)
                    } else {
                        write!(f, "{} km", v as f64 / 1000.0)
                    }
                } else {
                    write!(f, "{} m", v)
                }
            } else if v >= NM - 1 {
                if Self::near(v, NM) {
                    write!(f, "{} nm", (v + 1) / NM)
                } else {
                    write!(f, "{} nm", v as f64 / NM_F64)
                }
            } else if Self::near(v, FRAC_NM_2) {
                write!(f, "{}/2 nm", (v + 1) / FRAC_NM_2)
            } else if Self::near(v, FRAC_NM_4) {
                write!(f, "{}/4 nm", (v + 1) / FRAC_NM_4)
            } else if Self::near(v, FRAC_NM_8) {
                write!(f, "{}/8 nm", (v + 1) / FRAC_NM_8)
            } else if Self::near(v, FRAC_NM_16) {
                write!(f, "{}/16 nm", (v + 1) / FRAC_NM_16)
            } else if Self::near(v, FRAC_NM_32) {
                write!(f, "{}/32 nm", (v + 1) / FRAC_NM_32)
            } else {
                write!(f, "{} nm", v as f64 / NM_F64)
            }
        }
    }
}

impl From<Range> for i32 {
    fn from(range: Range) -> Self {
        range.distance
    }
}
impl From<&Range> for i32 {
    fn from(range: &Range) -> Self {
        range.distance
    }
}
#[derive(Debug, Clone)]
pub struct Ranges {
    pub all: Vec<Range>,
    pub metric: Vec<Range>,
    pub nautical: Vec<Range>,
    pub mixed: Vec<Range>,
    /// Only filled by a brand that can range in statute miles; not in `all`,
    /// which holds the metric and nautical ranges the others are told from.
    pub statute: Vec<Range>,
    /// Only filled by a brand that can range in kiloyards; not in `all`.
    pub kiloyards: Vec<Range>,
    ordered: bool,
}

impl Ranges {
    pub(crate) fn new(mut ranges: Vec<Range>) -> Self {
        let mut metric = Vec::new();
        let mut nautical = Vec::new();
        let mut mixed = Vec::new();
        let mut all = Vec::new();

        ranges.sort();
        for (i, range) in ranges.iter().enumerate() {
            if Range::metric(range.distance) {
                metric.push(Range::new(range.distance, i));
                if (range.distance) <= 1000 {
                    mixed.push(Range::new(range.distance, i));
                }
            } else {
                nautical.push(Range::new(range.distance, i));
                if (range.distance) > 1500 {
                    mixed.push(Range::new(range.distance, i));
                }
            }
            all.push(Range::new(range.distance, i));
        }
        Self {
            all: ranges,
            metric,
            nautical,
            mixed,
            statute: Vec::new(),
            kiloyards: Vec::new(),
            ordered: true,
        }
    }

    /// These ranges, plus the given distances in statute miles and in
    /// kiloyards, each in metres.
    pub(crate) fn with_statute_and_kiloyards(mut self, statute: &[i32], kiloyards: &[i32]) -> Self {
        let tagged = |distances: &[i32], unit| {
            let mut ranges: Vec<Range> = distances
                .iter()
                .enumerate()
                .map(|(index, &distance)| Range {
                    distance,
                    index,
                    unit,
                })
                .collect();
            ranges.sort();
            ranges
        };
        self.statute = tagged(statute, Unit::StatuteMiles);
        self.kiloyards = tagged(kiloyards, Unit::Kiloyards);
        self
    }

    pub(crate) fn empty() -> Self {
        Self {
            all: Vec::new(),
            metric: Vec::new(),
            nautical: Vec::new(),
            mixed: Vec::new(),
            statute: Vec::new(),
            kiloyards: Vec::new(),
            ordered: false,
        }
    }

    /// Build a range list from all known metric and nautical distances
    /// that fall within [min_m, max_m]. Used when the radar advertises
    /// its instrumented range directly (e.g. Navico 0xC409 TLV).
    pub(crate) fn from_range(min_m: i32, max_m: i32) -> Self {
        let mut distances: Vec<i32> = ALL_POSSIBLE_METRIC_RANGES
            .all
            .iter()
            .chain(ALL_POSSIBLE_NAUTICAL_RANGES.all.iter())
            .map(|r| r.distance)
            .filter(|&d| d >= min_m && d <= max_m)
            .collect();
        distances.sort();
        distances.dedup();
        Self::new_by_distance(&distances)
    }

    pub(crate) fn new_by_distance(distances: &[i32]) -> Self {
        let mut r = Vec::new();
        for (i, &value) in distances.iter().enumerate() {
            r.push(Range::new(value, i));
        }
        Self::new(r)
    }

    pub(crate) fn push(&mut self, range: Range) -> bool {
        if self.all.iter().any(|r| r.distance == range.distance) {
            // If the range already exists, do not add it again
            return false;
        }
        let index = self.all.len();
        self.all.push(range);
        if Range::metric(self.all[index].distance) {
            self.metric
                .push(Range::new(self.all[index].distance, index));
            if (range.distance) <= 1000 {
                self.mixed.push(Range::new(self.all[index].distance, index));
            }
        } else {
            self.nautical
                .push(Range::new(self.all[index].distance, index));
            if (range.distance) > 1500 {
                self.mixed.push(Range::new(self.all[index].distance, index));
            }
        }
        true
    }

    pub(crate) fn get_distance(&self, index: usize) -> i32 {
        if let Some(range) = self.all.get(index) {
            range.distance
        } else {
            0 // Return 0 if the index is out of bounds
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.all.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.all.is_empty()
    }
}

impl Display for Ranges {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let mut first = true;
        for range in &self.all {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{}", range)?;
            if !self.ordered && range.index > 0 {
                write!(f, " ")?;
            }
            first = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Range;

    // All Furuno range tables must classify correctly. A regression here
    // pollutes the per-unit range list and produces a dead zoom step in the
    // GUI at whichever boundary the misclassified value lands.

    const DRS_NXT_NM: &[i32] = &[
        116, 231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632, 44448,
        59264, 66672, 88896,
    ];
    const DRS_NXT_EXTENDED_NM: &[i32] = &[
        116, 231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632, 44448,
        59264, 66672, 88896, 118528, 133344, 177792,
    ];
    const DRS_NM: &[i32] = &[
        116, 231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632, 44448,
        59264, 66672,
    ];
    const FAR_NM: &[i32] = &[
        231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632, 44448,
        59264, 88896, 177792,
    ];
    const DRS_NXT_KM: &[i32] = &[
        125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000, 32000,
        36000, 48000, 64000,
    ];
    const DRS_NXT_EXTENDED_KM: &[i32] = &[
        125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000, 32000,
        36000, 48000, 64000, 96000, 120000, 160000,
    ];
    const DRS_KM: &[i32] = &[
        125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000, 32000,
        36000, 48000, 64000, 72000, 96000,
    ];
    const FAR_KM: &[i32] = &[
        125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000, 32000,
        48000, 96000,
    ];

    fn assert_all_nautical(table: &[i32], name: &str) {
        for &v in table {
            assert!(
                !Range::is_metric_distance(v),
                "{}: {}m should be nautical, was classified as metric",
                name,
                v
            );
        }
    }

    fn assert_all_metric(table: &[i32], name: &str) {
        for &v in table {
            assert!(
                Range::is_metric_distance(v),
                "{}: {}m should be metric, was classified as nautical",
                name,
                v
            );
        }
    }

    #[test]
    fn nautical_tables_classify_as_nautical() {
        assert_all_nautical(DRS_NXT_NM, "DRS_NXT_NM");
        assert_all_nautical(DRS_NXT_EXTENDED_NM, "DRS_NXT_EXTENDED_NM");
        assert_all_nautical(DRS_NM, "DRS_NM");
        assert_all_nautical(FAR_NM, "FAR_NM");
    }

    #[test]
    fn metric_tables_classify_as_metric() {
        assert_all_metric(DRS_NXT_KM, "DRS_NXT_KM");
        assert_all_metric(DRS_NXT_EXTENDED_KM, "DRS_NXT_EXTENDED_KM");
        assert_all_metric(DRS_KM, "DRS_KM");
        assert_all_metric(FAR_KM, "FAR_KM");
    }

    #[test]
    fn regression_125m_classifies_as_metric() {
        // Without the explicit special case in metric(), 125 % 50 == 25 and
        // the near-multiple-of-50 heuristic misclassifies 125m as nautical,
        // pushing it between 116m and 231m in the nautical range list and
        // producing a dead zoom step at the 1/16 NM minimum.
        assert!(Range::is_metric_distance(125));
    }

    /// Statute miles and kiloyards are not told from metres, so a range
    /// given in them is shown in them: 3,219 m would otherwise read as
    /// 1.74 nm and 1,829 m as 0.99 nm.
    #[test]
    fn statute_miles_and_kiloyards_show_in_their_own_units() {
        let ranges = super::Ranges::new_by_distance(&[1852])
            .with_statute_and_kiloyards(&[3219, 100, 201, 1207, 2414], &[114, 1829, 1372]);
        let shown = |ranges: &[Range]| ranges.iter().map(|r| r.to_string()).collect::<Vec<_>>();

        assert_eq!(
            shown(&ranges.statute),
            ["1/16 mi", "1/8 mi", "3/4 mi", "1.5 mi", "2 mi"]
        );
        assert_eq!(shown(&ranges.kiloyards), ["0.125 kyd", "1.5 kyd", "2 kyd"]);
        assert_eq!(shown(&ranges.nautical), ["1 nm"]);
        assert_eq!(ranges.all.len(), 1, "the other units stay out of all");
    }
}
