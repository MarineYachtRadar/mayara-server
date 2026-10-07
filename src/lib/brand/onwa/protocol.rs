//! ONWA radar protocol (KRA-5001 with the K-ASTRAL chartplotters) — wire format.
//!
//! Reverse engineered from captures of a K-ASTRAL 8 controlling a KRA-5001.
//!
//! ## Transport
//!
//! Everything is UDP. Commands are ASCII lines sent unicast to the radar's
//! [`COMMAND_PORT`]; the radar accepts them from any host. Everything the
//! radar sends is a broadcast to 255.255.255.255:
//!
//! - [`SPOKE_PORT`]: one spoke per datagram, each datagram sent twice
//! - [`REPLY_PORT`]: ASCII `#ACMD,$...` lines echoing every command
//!   (`$RDACK,<command>`) and answering queries
//! - [`STATE_PORT`]: binary `#ACMD,$RDANT,` and `#ACMD,$GAINS,` state
//!   reports, about once a second
//!
//! ## Commands
//!
//! `$NAME,args*\r\n`. The chartplotter sends no checksum after the `*`,
//! and for sea and rain doubles the `*`; the radar echoes whatever it gets.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

use crate::radar::NM;

// =============================================================================
// Network
// =============================================================================

pub(crate) const COMMAND_PORT: u16 = 3367;
pub(crate) const SPOKE_PORT: u16 = 7203;
pub(crate) const REPLY_PORT: u16 = 7204;
pub(crate) const STATE_PORT: u16 = 3823;

/// The state reports carry the radar's own address and MAC, so they double
/// as the discovery beacon.
pub(crate) const BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), STATE_PORT);
pub(crate) const SPOKE_ADDRESS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::BROADCAST, SPOKE_PORT);
pub(crate) const REPLY_ADDRESS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::BROADCAST, REPLY_PORT);
pub(crate) const STATE_ADDRESS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::BROADCAST, STATE_PORT);

// =============================================================================
// Spokes
// =============================================================================

/// Spoke angles are tenths of a degree from the bow, clockwise.
pub(crate) const SPOKES: usize = 3600;
const SPOKE_HEADER_LEN: usize = 4;
const WIRE_SAMPLES: usize = 996;
/// The radar's spoke reaches three times the selected range; the chartplotter
/// draws the rest only in the screen corners. Only the selected range is kept.
pub(crate) const SPOKE_LEN: usize = WIRE_SAMPLES / 3;
/// The radar sends 8-bit samples; halved to leave room for history colours.
pub(crate) const PIXEL_VALUES: u8 = 128;

pub(crate) struct Spoke<'a> {
    pub(crate) angle: u16,
    pub(crate) samples: &'a [u8],
}

pub(crate) fn parse_spoke(data: &[u8]) -> Option<Spoke<'_>> {
    if data.len() != SPOKE_HEADER_LEN + WIRE_SAMPLES {
        return None;
    }
    let angle = u16::from_le_bytes([data[0], data[1]]);
    if angle as usize >= SPOKES {
        return None;
    }
    Some(Spoke {
        angle,
        samples: &data[SPOKE_HEADER_LEN..SPOKE_HEADER_LEN + SPOKE_LEN],
    })
}

// =============================================================================
// Ranges
// =============================================================================

/// The radar's range index, in metres of the K-ASTRAL range menu.
pub(crate) const RANGES: [i32; 15] = [
    NM / 8,
    NM / 4,
    NM / 2,
    NM * 3 / 4,
    NM,
    NM * 3 / 2,
    NM * 2,
    NM * 3,
    NM * 4,
    NM * 6,
    NM * 8,
    NM * 12,
    NM * 16,
    NM * 24,
    NM * 36,
];

/// The pulse the chartplotter selects for each range index.
const PULSES: [u8; 15] = [0, 0, 1, 2, 2, 2, 2, 4, 4, 5, 5, 5, 5, 5, 5];

pub(crate) fn range_index_for(meters: i32) -> u8 {
    RANGES
        .iter()
        .enumerate()
        .min_by_key(|(_, r)| (**r - meters).abs())
        .map(|(i, _)| i as u8)
        .unwrap_or(0)
}

pub(crate) fn range_meters(index: u8) -> Option<i32> {
    RANGES.get(index as usize).copied()
}

// =============================================================================
// Levels
// =============================================================================

const LEVEL_MAX: f64 = 1023.;

/// Gain, sea and rain are sent as `1023 - level`, level being 0..=1023.
fn inverted_level(percent: f64) -> u16 {
    (LEVEL_MAX - (percent.clamp(0., 100.) * LEVEL_MAX / 100. + 0.5).floor()) as u16
}

fn level_percent(level: u16) -> f64 {
    (level as f64 * 100. / LEVEL_MAX).round()
}

// =============================================================================
// Commands
// =============================================================================

pub(crate) fn command(name: &str, args: &str) -> Vec<u8> {
    format!("${},{}*\r\n", name, args).into_bytes()
}

/// Sea and rain carry a second `*`, as the chartplotter sends them.
fn clutter_command(name: &str, percent: f64) -> Vec<u8> {
    format!("${},{}**\r\n", name, inverted_level(percent)).into_bytes()
}

pub(crate) fn gain_command(percent: f64) -> Vec<u8> {
    command("RXGAI", &inverted_level(percent).to_string())
}

pub(crate) fn sea_command(percent: f64) -> Vec<u8> {
    clutter_command("ACSEA", percent)
}

pub(crate) fn rain_command(percent: f64) -> Vec<u8> {
    clutter_command("ACRAI", percent)
}

pub(crate) fn power_commands(transmit: bool) -> [Vec<u8>; 2] {
    let on_off = if transmit { "ON" } else { "OFF" };
    [command("TXMIT", on_off), command("ANTSW", on_off)]
}

/// A range change, as the chartplotter sends it: three datagrams, each the
/// new range followed by a pulse, which steps from the old pulse through 0
/// to the new one.
pub(crate) fn range_commands(old_index: u8, new_index: u8) -> [Vec<u8>; 3] {
    let old = PULSES[(old_index as usize).min(PULSES.len() - 1)];
    let new = PULSES[(new_index as usize).min(PULSES.len() - 1)];
    [old, 0, new].map(|pulse| {
        let mut packet = command("TXRNG", &new_index.to_string());
        packet.extend(command("PSELN", &pulse.to_string()));
        packet
    })
}

/// The dead sector, in whole tenths of a degree: where it starts, and how
/// far it reaches clockwise.
pub(crate) fn dead_sector_command(start_deg: f64, end_deg: f64) -> Vec<u8> {
    let start = tenths(start_deg);
    let width = (tenths(end_deg) + SPOKES as u16 - start) % SPOKES as u16;
    command("DEAZN", &format!("{},{}", start, width))
}

fn tenths(degrees: f64) -> u16 {
    ((degrees * 10.).round() as i32).rem_euclid(SPOKES as i32) as u16
}

/// The chartplotter polls these every second; the radar answers the two
/// queries with its software and firmware versions.
pub(crate) fn keepalive_commands(timer: u32) -> [Vec<u8>; 3] {
    [
        command("TIMER", &timer.to_string()),
        b"$GETVE,0*1A\r\n".to_vec(),
        command("GETFV", "0"),
    ]
}

// =============================================================================
// State reports
// =============================================================================

const RDANT_PREFIX: &[u8] = b"#ACMD,$RDANT,";
const GAINS_PREFIX: &[u8] = b"#ACMD,$GAINS,";

const RDANT_MAC: usize = 20;
const RDANT_NETMASK: usize = 26;
const RDANT_SECTOR_START: usize = 40;
const RDANT_SECTOR_WIDTH: usize = 42;
const RDANT_SECTOR_ON: usize = 44;
const RDANT_RANGE: usize = 45;
const RDANT_ECHO_STRETCH: usize = 48;
const RDANT_INTERFERENCE: usize = 49;
const RDANT_NOISE: usize = 50;
const RDANT_SWEEP_TIMING: usize = 56;
const RDANT_TRANSMIT: usize = 58;
const RDANT_LEN: usize = 60;

const GAINS_RAIN: usize = 0;
const GAINS_SEA: usize = 2;
const GAINS_GAIN: usize = 4;
const GAINS_LEN: usize = 6;

/// The radar's `$RDANT` report: its identity and nearly every setting.
#[derive(Debug, PartialEq)]
pub(crate) struct State {
    pub(crate) mac: [u8; 6],
    pub(crate) netmask: Ipv4Addr,
    /// Dead sector start and end, tenths of a degree.
    pub(crate) dead_sector: (u16, u16),
    pub(crate) dead_sector_on: bool,
    pub(crate) range_index: u8,
    pub(crate) echo_stretch: u8,
    pub(crate) interference_rejection: u8,
    pub(crate) noise_rejection: u8,
    pub(crate) sweep_timing: u16,
    pub(crate) transmit: bool,
}

pub(crate) fn parse_state(data: &[u8]) -> Option<State> {
    let b = data.strip_prefix(RDANT_PREFIX)?;
    if b.len() < RDANT_LEN {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let start = u16_at(RDANT_SECTOR_START) % SPOKES as u16;
    let width = u16_at(RDANT_SECTOR_WIDTH) % SPOKES as u16;
    let mut mac = [0; 6];
    mac.copy_from_slice(&b[RDANT_MAC..RDANT_MAC + 6]);
    Some(State {
        mac,
        netmask: Ipv4Addr::new(
            b[RDANT_NETMASK],
            b[RDANT_NETMASK + 1],
            b[RDANT_NETMASK + 2],
            b[RDANT_NETMASK + 3],
        ),
        dead_sector: (start, (start + width) % SPOKES as u16),
        dead_sector_on: b[RDANT_SECTOR_ON] != 0,
        range_index: b[RDANT_RANGE],
        echo_stretch: b[RDANT_ECHO_STRETCH],
        interference_rejection: b[RDANT_INTERFERENCE],
        noise_rejection: b[RDANT_NOISE],
        sweep_timing: u16_at(RDANT_SWEEP_TIMING),
        transmit: b[RDANT_TRANSMIT] != 0,
    })
}

/// The radar's `$GAINS` report, in percent.
#[derive(Debug, PartialEq)]
pub(crate) struct Gains {
    pub(crate) gain: f64,
    pub(crate) sea: f64,
    pub(crate) rain: f64,
}

pub(crate) fn parse_gains(data: &[u8]) -> Option<Gains> {
    let b = data.strip_prefix(GAINS_PREFIX)?;
    if b.len() < GAINS_LEN {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    // Gain and sea come back on the natural scale, rain as it was sent.
    Some(Gains {
        gain: level_percent(u16_at(GAINS_GAIN)),
        sea: level_percent(u16_at(GAINS_SEA)),
        rain: 100. - level_percent(u16_at(GAINS_RAIN)),
    })
}

// =============================================================================
// Replies
// =============================================================================

const REPLY_PREFIX: &[u8] = b"#ACMD,$";

/// A reply line's name and arguments: `#ACMD,$RDACK,$TXRNG,3*` gives
/// `("RDACK", "$TXRNG,3")`.
pub(crate) fn parse_reply(data: &[u8]) -> Option<(&str, &str)> {
    let line = std::str::from_utf8(data.strip_prefix(REPLY_PREFIX)?).ok()?;
    let line = line.trim_end_matches(['\0', '\r', '\n']);
    let line = line.split_once('*').map_or(line, |(l, _)| l);
    Some(line.split_once(',').unwrap_or((line, "")))
}

/// The range index from an echoed `$TXRNG`, which arrives well before the
/// next state report.
pub(crate) fn acked_range(args: &str) -> Option<u8> {
    args.strip_prefix("$TXRNG,")?.parse().ok()
}

/// The firmware names the model: `KR5001.ES.2K.V1.00.190623` is a KRA-5001.
pub(crate) fn model_from_firmware(firmware: &str) -> Option<String> {
    let digits: String = firmware
        .strip_prefix("KR")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then(|| format!("KRA-{}", digits))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `$RDANT` report as captured from a KRA-5001 at 0.75 nm, transmitting,
    /// dead sector 45°–180.8° switched on.
    fn rdant() -> Vec<u8> {
        let mut p = RDANT_PREFIX.to_vec();
        p.extend_from_slice(&[0x00]);
        p.extend_from_slice(b"RA-ANT-001\0");
        p.extend_from_slice(&[223, 168, 1, 128, 223, 168, 1, 168]);
        p.extend_from_slice(&[0x00, 0x30, 0x6c, 0x00, 0x00, 0x29]);
        p.extend_from_slice(&[255, 255, 255, 0, 223, 168, 1, 1]);
        p.extend_from_slice(&[0x5b, 0xcb, 0x84, 0x84, 0xbf, 0x04]);
        p.extend_from_slice(&[0xc2, 0x01, 0x4e, 0x05, 0x01]);
        p.extend_from_slice(&[0x03, 0x02, 0x03, 0x01, 0x03, 0x00, 0x00, 0x01, 0x00]);
        p.extend_from_slice(&[0xe8, 0x03, 0x23, 0x00, 0x01, 0x01]);
        p.extend_from_slice(&[0; 20]);
        p
    }

    #[test]
    fn state_report_carries_identity_and_settings() {
        let state = parse_state(&rdant()).unwrap();
        assert_eq!(state.mac, [0x00, 0x30, 0x6c, 0x00, 0x00, 0x29]);
        assert_eq!(state.netmask, Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(state.dead_sector, (450, 1808));
        assert!(state.dead_sector_on);
        assert_eq!(state.range_index, 3);
        assert_eq!(state.echo_stretch, 1);
        assert_eq!(state.interference_rejection, 3);
        assert_eq!(state.noise_rejection, 0);
        assert_eq!(state.sweep_timing, 35);
        assert!(state.transmit);
    }

    #[test]
    fn truncated_state_report_is_refused() {
        let mut p = rdant();
        p.truncate(RDANT_PREFIX.len() + RDANT_LEN - 1);
        assert_eq!(parse_state(&p), None);
        assert_eq!(parse_state(b"#ACMD,$GAINS,\xff\x03"), None);
    }

    /// Captured with gain 68, sea 0, rain 0 on the chartplotter.
    #[test]
    fn gains_report_in_percent() {
        let mut p = GAINS_PREFIX.to_vec();
        p.extend_from_slice(&[0xff, 0x03, 0x00, 0x00, 0xb8, 0x02, 0x01, 0x00]);
        assert_eq!(
            parse_gains(&p),
            Some(Gains {
                gain: 68.,
                sea: 0.,
                rain: 0.
            })
        );
    }

    /// Values the chartplotter sent during 1 % sweeps of each control.
    #[test]
    fn levels_match_the_chartplotter() {
        assert_eq!(gain_command(90.), b"$RXGAI,102*\r\n");
        assert_eq!(gain_command(68.), b"$RXGAI,327*\r\n");
        assert_eq!(gain_command(50.), b"$RXGAI,511*\r\n");
        assert_eq!(gain_command(0.), b"$RXGAI,1023*\r\n");
        assert_eq!(gain_command(100.), b"$RXGAI,0*\r\n");
        assert_eq!(sea_command(1.), b"$ACSEA,1013**\r\n");
        assert_eq!(rain_command(10.), b"$ACRAI,921**\r\n");
    }

    #[test]
    fn range_change_steps_the_pulse_through_zero() {
        let [a, b, c] = range_commands(2, 3);
        assert_eq!(a, b"$TXRNG,3*\r\n$PSELN,1*\r\n");
        assert_eq!(b, b"$TXRNG,3*\r\n$PSELN,0*\r\n");
        assert_eq!(c, b"$TXRNG,3*\r\n$PSELN,2*\r\n");
    }

    #[test]
    fn range_index_is_the_nearest_menu_range() {
        assert_eq!(range_index_for(NM / 8), 0);
        assert_eq!(range_index_for(1400), 3);
        assert_eq!(range_index_for(NM * 36), 14);
        assert_eq!(range_index_for(NM * 100), 14);
        assert_eq!(range_meters(3), Some(1389));
        assert_eq!(range_meters(15), None);
    }

    /// The chartplotter sent `$DEAZN,450,1358*` for a 45°–180.8° sector.
    #[test]
    fn dead_sector_is_start_and_width() {
        assert_eq!(dead_sector_command(45., 180.8), b"$DEAZN,450,1358*\r\n");
        assert_eq!(dead_sector_command(-60., 60.), b"$DEAZN,3000,1200*\r\n");
    }

    #[test]
    fn spoke_angle_and_samples() {
        let mut p = vec![0x1e, 0x04, 0xb4, 0x70];
        p.extend_from_slice(&[7; WIRE_SAMPLES]);
        let spoke = parse_spoke(&p).unwrap();
        assert_eq!(spoke.angle, 1054);
        assert_eq!(spoke.samples.len(), 332, "the selected range only");

        p[1] = 0x0e; // angle 3614 is past a full turn
        assert!(parse_spoke(&p).is_none());
        assert!(parse_spoke(&p[..100]).is_none());
    }

    #[test]
    fn replies_split_into_name_and_arguments() {
        assert_eq!(
            parse_reply(b"#ACMD,$RDACK,$TXRNG,3*\r\n\0\0"),
            Some(("RDACK", "$TXRNG,3"))
        );
        assert_eq!(
            parse_reply(b"#ACMD,$ANTFV,KR5001.ES.2K.V1.00.190623\r\n"),
            Some(("ANTFV", "KR5001.ES.2K.V1.00.190623"))
        );
        assert_eq!(parse_reply(b"#AACK\0"), None);
        assert_eq!(acked_range("$TXRNG,11"), Some(11));
        assert_eq!(acked_range("$ESLVL,1"), None);
    }

    #[test]
    fn model_is_named_by_the_firmware() {
        assert_eq!(
            model_from_firmware("KR5001.ES.2K.V1.00.190623").as_deref(),
            Some("KRA-5001")
        );
        assert_eq!(model_from_firmware("V6.8 2022-08-29"), None);
    }
}
