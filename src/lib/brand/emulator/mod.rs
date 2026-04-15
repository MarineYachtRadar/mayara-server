use std::f64::consts::PI;
use std::net::{Ipv4Addr, SocketAddrV4};

use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::config::GuardZone;
use crate::locator::LocatorAddress;
use crate::radar::settings::ControlId;
use crate::radar::{GeoPosition, RadarInfo, SharedRadars};
use crate::{Brand, Cli};

mod command;
mod report;
mod settings;
mod world;

// Like HALO radar
const EMULATOR_SPOKES: usize = 2048;
const EMULATOR_SPOKE_LEN: usize = 1024;

// Initial position: Sneek, Netherlands
const EMULATOR_INITIAL_LAT: f64 = 53.18433606795611;
const EMULATOR_INITIAL_LON: f64 = 5.273436414539474;
const EMULATOR_HEADING: f64 = 270.0; // West

// Speed in knots (nautical miles per hour)
const EMULATOR_SPEED_KNOTS: f64 = 2.0;

// Supported ranges in meters
pub(crate) const EMULATOR_RANGES: &[i32] = &[
    50, 57, 75, 100, 115, 231, 250, 463, 500, 750, 926, 1000, 1389, 1500, 1852, 2000, 2778, 3000,
    3704, 4000, 5556, 6000, 7408, 8000, 11112, 12000, 14816, 16000, 22224, 24000, 29632, 36000,
    44448, 48000, 59264, 64000, 66672, 72000, 74080, 88896,
];

/// Get the initial position for the emulator, considering static-position/stationary override
pub(crate) fn get_initial_position(args: &Cli) -> (GeoPosition, f64, f64) {
    if let Some(static_pos) = args.get_static_position() {
        // Use static position with speed = 0
        (
            GeoPosition::new(static_pos.lat, static_pos.lon),
            static_pos.heading,
            0.0, // Speed is 0 when using static position
        )
    } else if args.stationary {
        // Stationary mode: use default position but speed = 0
        (
            GeoPosition::new(EMULATOR_INITIAL_LAT, EMULATOR_INITIAL_LON),
            EMULATOR_HEADING,
            0.0, // Speed is 0 in stationary mode
        )
    } else {
        (
            GeoPosition::new(EMULATOR_INITIAL_LAT, EMULATOR_INITIAL_LON),
            EMULATOR_HEADING,
            EMULATOR_SPEED_KNOTS,
        )
    }
}

/// Register emulator brand listener (not used for actual discovery)
pub(super) fn new(_args: &Cli, _addresses: &mut Vec<LocatorAddress>) {
    // The emulator doesn't use the normal locator discovery mechanism.
    // Instead, create_emulator_radar() is called directly from the locator.
    log::info!("Emulator mode enabled");
}

/// Create the emulator radar directly (called from locator when --emulator is set)
pub(crate) fn create_emulator_radar(args: &Cli, radars: &SharedRadars, subsys: &SubsystemHandle) {
    log::info!("create_emulator_radar called");

    // Check if we already have an emulator radar
    if radars.is_radar_active_on_nic(&Brand::Emulator, &Ipv4Addr::LOCALHOST) {
        log::debug!("Emulator radar already exists");
        return;
    }

    let fake_addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);

    let info = RadarInfo::new(
        radars,
        args,
        Brand::Emulator,
        Some("EMU00001"),
        None,
        16, // 16 pixel values (4 bits like HALO)
        EMULATOR_SPOKES,
        EMULATOR_SPOKE_LEN,
        fake_addr,
        Ipv4Addr::LOCALHOST,
        fake_addr, // spoke_data_addr (unused)
        fake_addr, // report_addr (unused)
        fake_addr, // send_command_addr (unused)
        |id, tx| settings::new(id, tx, args),
        true,  // doppler (like HALO)
        false, // sparse_spokes
    );

    if let Some(mut info) = radars.add(info) {
        log::info!("Emulator radar '{}' created", info.key());

        // Set the ranges
        let ranges = crate::radar::range::Ranges::new_by_distance(
            &EMULATOR_RANGES.iter().map(|&r| r).collect::<Vec<_>>(),
        );
        info.set_ranges(ranges);

        // Set a default guard zone for testing if none was loaded from persistence
        // Covers the rear starboard quadrant: -135° to -45° relative to heading (225° to 315°)
        if info.controls.guard_zone(&ControlId::GuardZone1).is_none() {
            let guard_zone = GuardZone {
                start_angle: 225.0 * PI / 180.0,
                end_angle: 315.0 * PI / 180.0,
                start_distance: 300.0,
                end_distance: 700.0,
                enabled: true,
            };
            info.controls
                .set_guard_zone(&ControlId::GuardZone1, &guard_zone);
            log::info!("Emulator: Set default guard zone 1 for ARPA testing");
        }

        // Set guard zone 2 to catch fast eastbound targets (40 knots, 300m north)
        // Covers northwest quadrant: 270° to 360° (west to north) to catch them as they appear
        if info.controls.guard_zone(&ControlId::GuardZone2).is_none() {
            let guard_zone = GuardZone {
                start_angle: 270.0 * PI / 180.0,
                end_angle: 360.0 * PI / 180.0,
                start_distance: 200.0,
                end_distance: 500.0,
                enabled: true,
            };
            info.controls
                .set_guard_zone(&ControlId::GuardZone2, &guard_zone);
            log::info!("Emulator: Set default guard zone 2 for fast target testing");
        }

        radars.update(&mut info);

        // Start the report receiver (spoke generator)
        let report_name = info.key() + " reports";
        let report_receiver = report::EmulatorReportReceiver::new(args, info, radars.clone());

        subsys.start(SubsystemBuilder::new(report_name, |s| {
            report_receiver.run(s)
        }));
    }
}
