use std::collections::HashMap;
use std::fmt::{self, Display};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::brand::{LocatorId, RadarLocator};
use crate::locator::LocatorAddress;
use crate::radar::range::Ranges;
use crate::radar::{RadarInfo, SharedRadars};
use crate::{Brand, Cli};

mod capabilities;
mod command;
mod discovery;
mod protocol;
mod range_table;
mod report;
mod settings;

use capabilities::GarminCapabilities;
use protocol::*;

// 1 nautical mile in meters
const NM: i32 = 1852;

// Garmin HD metric ranges (meters)
const GARMIN_HD_RANGES_METRIC: &[i32] = &[
    250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000, 36000, 48000,
    64000,
];

// Garmin HD nautical ranges (meters, based on NM fractions)
const GARMIN_HD_RANGES_NAUTICAL: &[i32] = &[
    232,        // ~1/8 NM
    NM / 4,     // 463
    NM / 2,     // 926
    NM * 3 / 4, // 1389
    NM,         // 1852
    NM * 3 / 2, // 2778
    NM * 2,     // 3704
    NM * 3,     // 5556
    NM * 4,     // 7408
    NM * 6,     // 11112
    NM * 8,     // 14816
    NM * 12,    // 22224
    NM * 16,    // 29632
    NM * 24,    // 44448
    NM * 36,    // 66672
    NM * 48,    // 88896
];

/// Supported Garmin radar types
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum GarminRadarType {
    /// Original HD radar: 720 spokes, 1-bit samples
    HD,
    /// xHD radar: 1440 spokes, 8-bit samples
    XHD,
    /// xHD2 radar: NOT YET SUPPORTED (different protocol)
    XHD2,
    /// xHD3 radar: NOT YET SUPPORTED (different protocol)
    XHD3,
    /// Fantom radar: NOT YET SUPPORTED (different protocol)
    Fantom,
}

impl Display for GarminRadarType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s: &'static str = match self {
            GarminRadarType::HD => "HD",
            GarminRadarType::XHD => "xHD",
            GarminRadarType::XHD2 => "xHD2",
            GarminRadarType::XHD3 => "xHD3",
            GarminRadarType::Fantom => "Fantom",
        };
        write!(f, "{}", s)
    }
}

impl GarminRadarType {
    /// Returns the number of spokes per revolution for this radar type
    pub(crate) fn spokes_per_revolution(&self) -> usize {
        match self {
            GarminRadarType::HD => HD_SPOKES_PER_REVOLUTION,
            // Unsupported types default to enhanced protocol specs
            _ => SPOKES_PER_REVOLUTION,
        }
    }

    /// Returns the maximum spoke length for this radar type
    pub(crate) fn max_spoke_len(&self) -> usize {
        match self {
            GarminRadarType::HD => HD_MAX_SPOKE_LEN,
            _ => MAX_SPOKE_LEN,
        }
    }

    /// Returns the number of pixel values for this radar type
    pub(crate) fn pixel_values(&self) -> u8 {
        match self {
            GarminRadarType::HD => HD_PIXEL_VALUES,
            _ => PIXEL_VALUES,
        }
    }

    /// Returns true if this radar type is currently supported
    pub(crate) fn is_supported(&self) -> bool {
        matches!(self, GarminRadarType::HD | GarminRadarType::XHD)
    }
}

/// State the locator keeps for each Garmin radar IP it has seen.
///
/// A radar must broadcast its capability
/// bitmap (`MSG_CAPABILITY`) and its range table (`MSG_RANGE_TABLE`)
/// before the MFD enables any UI controls. We mirror that: an enhanced-protocol radar
/// lives in `Pending` until both arrive; HD radars synthesize the
/// capability bitmap and use a hardcoded range table, so they skip
/// straight to `Registered`.
#[derive(Clone, Debug)]
enum RadarState {
    /// Enhanced-protocol radar detected, waiting for `MSG_CAPABILITY` and
    /// `MSG_RANGE_TABLE` before registering. The detected_type is
    /// always `XHD` here.
    Pending {
        detected_type: GarminRadarType,
        capabilities: Option<GarminCapabilities>,
        ranges: Option<Ranges>,
    },
    /// Radar has been registered with mayara and is being driven by its
    /// own `GarminReportReceiver` subsystem.
    Registered,
}

/// Shared state across the two `GarminLocator` clones (one bound to the
/// CDM heartbeat group, one bound to the report group). Both locator
/// instances see different parts of the same radar's traffic — the CDM
/// heartbeat carries the `product_id`, the report stream carries the
/// capability bitmap and range table — so they cooperate via this
/// shared map.
#[derive(Default)]
struct LocatorShared {
    radars: HashMap<SocketAddrV4, RadarState>,
    /// Garmin CDM `product_id` per radar IP, learned from the `0x038e`
    /// heartbeat. Used to look up the model name.
    product_ids: HashMap<SocketAddrV4, u16>,
    /// Factory name + user alias per radar IP, learned from the `0x0392`
    /// product data response.
    device_names: HashMap<SocketAddrV4, discovery::CdmProductData>,
}

#[derive(Clone)]
struct GarminLocator {
    args: Cli,
    state: Arc<Mutex<LocatorShared>>,
}

impl GarminLocator {
    fn new(args: Cli) -> Self {
        GarminLocator {
            args,
            state: Arc::new(Mutex::new(LocatorShared::default())),
        }
    }

    fn found(
        &self,
        info: RadarInfo,
        info_b: Option<RadarInfo>,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        if let Some(mut info) = radars.add(info) {
            info.start_forwarding_radar_messages_to_stdout(subsys);

            let report_name = info.key();
            radars.update(&mut info);

            let mut report_receiver =
                report::GarminReportReceiver::new(&self.args, info, radars.clone());

            // Attach Range B if dual-range
            if let Some(ib) = info_b {
                if let Some(mut ib) = radars.add(ib) {
                    ib.start_forwarding_radar_messages_to_stdout(subsys);
                    radars.update(&mut ib);
                    report_receiver.set_range_b(&self.args, ib, radars.clone());
                }
            }

            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }

    /// Detect radar type from packet type
    fn detect_radar_type(packet_type: u32) -> Option<GarminRadarType> {
        match packet_type {
            MSG_HD_SPOKE | MSG_HD_STATE | MSG_HD_SETTINGS => Some(GarminRadarType::HD),
            MSG_RPM_MODE
            | MSG_TRANSMIT_MODE
            | MSG_RANGE_A
            | MSG_RANGE_A_GAIN_MODE
            | MSG_RANGE_A_GAIN
            | MSG_RANGE_A_AUTO_LEVEL
            | MSG_BEARING_ALIGNMENT
            | MSG_NOISE_BLANKER
            | MSG_RANGE_A_RAIN_MODE
            | MSG_RANGE_A_RAIN_GAIN
            | MSG_RANGE_A_SEA_MODE
            | MSG_RANGE_A_SEA_GAIN
            | MSG_RANGE_A_SEA_STATE
            | MSG_NO_TX_ZONE_1_MODE
            | MSG_NO_TX_ZONE_1_START
            | MSG_NO_TX_ZONE_1_STOP
            | MSG_SENTRY_MODE
            | MSG_SENTRY_STANDBY_TIME
            | MSG_SENTRY_TRANSMIT_TIME
            | MSG_SCANNER_STATE
            | MSG_STATE_CHANGE
            | protocol::MSG_ERROR_MESSAGE => Some(GarminRadarType::XHD),
            _ => None,
        }
    }

    fn process_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> io::Result<()> {
        if report.len() < GMN_HEADER_LEN {
            return Ok(());
        }

        let packet_type = u32::from_le_bytes(report[0..4].try_into().unwrap());

        // CDM messages (broadcast on a separate multicast group).
        if packet_type == MSG_CDM_HEARTBEAT {
            self.handle_cdm_heartbeat(report, from);
            return Ok(());
        }
        if packet_type == discovery::MSG_CDM_PRODUCT_DATA {
            self.handle_cdm_product_data(report, from);
            return Ok(());
        }

        // Already-registered radars are driven by their own report receiver
        // subsystem; the locator just ignores their packets so we don't
        // double-process anything.
        if matches!(
            self.state.lock().unwrap().radars.get(from),
            Some(RadarState::Registered)
        ) {
            return Ok(());
        }

        // The two pending-state triggers: capability bitmap and range table.
        // Either can arrive in any order; once both are present we register.
        if packet_type == MSG_CAPABILITY {
            self.handle_capability(report, from, nic_addr, radars, subsys);
            return Ok(());
        }
        if packet_type == MSG_RANGE_TABLE {
            self.handle_range_table(report, from, nic_addr, radars, subsys);
            return Ok(());
        }

        let detected_type = match Self::detect_radar_type(packet_type) {
            Some(t) => t,
            None => return Ok(()),
        };

        if !detected_type.is_supported() {
            log::warn!(
                "{}: Detected unsupported Garmin radar type: {}",
                from,
                detected_type
            );
            return Ok(());
        }

        match detected_type {
            GarminRadarType::HD => {
                // HD radars never broadcast a capability bitmap or range
                // table, so we synthesize both from
                // fixed legacy values and register on the first packet.
                let already_known = {
                    let state = self.state.lock().unwrap();
                    state.radars.contains_key(from)
                };
                if !already_known {
                    log::info!("{}: Detected Garmin HD radar via {}", from, nic_addr);
                    let caps = GarminCapabilities::for_legacy_hd();
                    let ranges = hd_ranges();
                    self.register(detected_type, caps, ranges, from, nic_addr, radars, subsys);
                }
            }
            _ => {
                // Enhanced protocol: stash a pending entry and wait for 0x09B1 + 0x09B2.
                let mut state = self.state.lock().unwrap();
                if !state.radars.contains_key(from) {
                    log::info!(
                        "{}: Detected Garmin {} radar via {}, waiting for capability bitmap and range table",
                        from,
                        detected_type,
                        nic_addr
                    );
                    state.radars.insert(
                        *from,
                        RadarState::Pending {
                            detected_type,
                            capabilities: None,
                            ranges: None,
                        },
                    );
                }
            }
        }

        Ok(())
    }

    /// Parse a CDM `0x038E` heartbeat and stash the radar's product_id.
    /// On first receipt, also send a `0x0391` product data request to
    /// learn the radar's factory name and user alias.
    fn handle_cdm_heartbeat(&self, report: &[u8], from: &SocketAddrV4) {
        let payload = &report[GMN_HEADER_LEN..];
        let hb = match discovery::parse(payload) {
            Some(h) => h,
            None => return,
        };
        let mut state = self.state.lock().unwrap();
        let already = state.product_ids.get(from).copied();
        if already != Some(hb.product_id) {
            log::info!(
                "{}: Garmin CDM heartbeat: product_id=0x{:04x} ({})",
                from,
                hb.product_id,
                discovery::product_name(hb.product_id).unwrap_or("unknown"),
            );
            state.product_ids.insert(*from, hb.product_id);

            // Send a product data request to learn the device name/alias.
            // This is fire-and-forget; the response arrives as 0x0392 on
            // the same CDM multicast port and is handled by
            // handle_cdm_product_data().
            let request = discovery::build_product_data_request();
            let dest = std::net::SocketAddrV4::new(*from.ip(), CDM_HEARTBEAT_PORT);
            // Spawn a one-shot UDP send so we don't block the locator.
            tokio::spawn(async move {
                if let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await {
                    let _ = sock.send_to(&request, dest).await;
                }
            });
        }
    }

    /// Parse a CDM `0x0392` product data response and stash the radar's
    /// factory name and user alias.
    fn handle_cdm_product_data(&self, report: &[u8], from: &SocketAddrV4) {
        let payload = &report[GMN_HEADER_LEN..];
        let pd = match discovery::parse_product_data(payload) {
            Some(p) => p,
            None => return,
        };
        log::info!(
            "{}: CDM product data: name={:?} alias={:?}",
            from,
            pd.device_name,
            pd.device_alias,
        );
        let mut state = self.state.lock().unwrap();
        state.device_names.insert(*from, pd);
    }

    /// Handle a `MSG_CAPABILITY` packet that arrived for a pending
    /// radar (or for one we haven't seen any other packets from yet).
    fn handle_capability(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let payload = &report[GMN_HEADER_LEN..];
        let caps = match GarminCapabilities::parse(payload) {
            Some(c) => c,
            None => {
                log::warn!(
                    "{}: 0x09B1 capability message too short ({} bytes)",
                    from,
                    payload.len()
                );
                return;
            }
        };

        {
            let mut state = self.state.lock().unwrap();
            let entry = state.radars.entry(*from).or_insert(RadarState::Pending {
                detected_type: GarminRadarType::XHD,
                capabilities: None,
                ranges: None,
            });
            if let RadarState::Pending {
                capabilities,
                ranges,
                ..
            } = entry
            {
                *capabilities = Some(caps);
                log::info!(
                    "{}: capability bitmap received: dual_range={} motionscope={} \
                     sentry={} fantom={} (range_table_seen={})",
                    from,
                    caps.has_dual_range(),
                    caps.has_motionscope(),
                    caps.has_sentry_mode(),
                    caps.is_fantom(),
                    ranges.is_some(),
                );
            }
        }
        self.try_register_pending(from, nic_addr, radars, subsys);
    }

    /// Handle a `MSG_RANGE_TABLE` packet that arrived for a pending
    /// radar (or for one we haven't seen any other packets from yet).
    fn handle_range_table(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let payload = &report[GMN_HEADER_LEN..];
        let parsed = match range_table::parse(payload) {
            Some(r) => r,
            None => {
                log::warn!(
                    "{}: 0x09B2 range table malformed or too short ({} bytes)",
                    from,
                    payload.len()
                );
                return;
            }
        };

        {
            let mut state = self.state.lock().unwrap();
            let entry = state.radars.entry(*from).or_insert(RadarState::Pending {
                detected_type: GarminRadarType::XHD,
                capabilities: None,
                ranges: None,
            });
            if let RadarState::Pending {
                ranges,
                capabilities,
                ..
            } = entry
            {
                log::info!(
                    "{}: range table received ({} entries, capabilities_seen={})",
                    from,
                    parsed.all.len(),
                    capabilities.is_some(),
                );
                *ranges = Some(parsed);
            }
        }
        self.try_register_pending(from, nic_addr, radars, subsys);
    }

    /// If the pending entry for this IP has both capabilities and a
    /// range table, build a `RadarInfo` and register it.
    fn try_register_pending(
        &mut self,
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let (detected_type, caps, ranges) = {
            let state = self.state.lock().unwrap();
            match state.radars.get(from) {
                Some(RadarState::Pending {
                    detected_type,
                    capabilities: Some(caps),
                    ranges: Some(ranges),
                }) => (*detected_type, *caps, ranges.clone()),
                _ => return,
            }
        };
        self.register(detected_type, caps, ranges, from, nic_addr, radars, subsys);
    }

    /// Build a `RadarInfo` for the given radar and hand it off to the
    /// shared registry.
    fn register(
        &mut self,
        detected_type: GarminRadarType,
        capabilities: GarminCapabilities,
        ranges: Ranges,
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let radar_send = SocketAddrV4::new(*from.ip(), COMMAND_PORT);
        let report_addr = SocketAddrV4::new(Ipv4Addr::new(239, 254, 2, 0), REPORT_PORT);
        let spoke_data_addr = match detected_type {
            GarminRadarType::HD => report_addr,
            GarminRadarType::XHD => {
                SocketAddrV4::new(Ipv4Addr::new(239, 254, 2, 0), DATA_PORT)
            }
            _ => return,
        };

        // Garmin radars don't expose a per-unit serial number on the
        // wire. Passing None for serial_no makes RadarInfo::new fall
        // back to the last two octets of the radar's IP (172.16.x.y)
        // as the key suffix, which is unique per physical radar.
        let state = self.state.lock().unwrap();
        let product_id = state.product_ids.get(from).copied();
        let device_info = state.device_names.get(from).cloned();
        drop(state);

        // Model name: prefer the factory name from 0x0392, fall back
        // to the product_id lookup, then infer from capability bits.
        let model_name = device_info
            .as_ref()
            .filter(|d| !d.device_name.is_empty())
            .map(|d| d.device_name.clone())
            .or_else(|| {
                product_id
                    .and_then(discovery::product_name)
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| detect_model_name(detected_type, &capabilities));

        // User name: prefer the device alias from 0x0392 if it differs
        // from the factory name (meaning the user actually renamed it).
        let user_name = device_info
            .as_ref()
            .filter(|d| !d.device_alias.is_empty() && d.device_alias != d.device_name)
            .map(|d| d.device_alias.clone())
            .unwrap_or_else(|| model_name.clone());

        let has_dual_range = capabilities.has_dual_range();
        let has_doppler = capabilities.has_motionscope();

        // Range A (or the only range in single-range mode)
        let dual_a = if has_dual_range { Some("A") } else { None };
        let mut radar_info = RadarInfo::new(
            radars,
            &self.args,
            Brand::Garmin,
            None,
            dual_a,
            detected_type.pixel_values(),
            detected_type.spokes_per_revolution(),
            detected_type.max_spoke_len(),
            *from,
            nic_addr.clone(),
            spoke_data_addr,
            report_addr,
            radar_send,
            |id, tx| settings::new(id, tx, &self.args, detected_type, &capabilities, false),
            has_doppler,
            false,
        );
        if has_doppler {
            radar_info.set_doppler_levels(DOPPLER_LEVELS_PER_DIRECTION);
        }
        radar_info.controls.set_model_name(model_name.clone());
        radar_info.controls.set_user_name(user_name.clone());
        radar_info.set_ranges(ranges.clone());

        // Range B (dual-range only)
        let info_b = if has_dual_range {
            let mut info_b = RadarInfo::new(
                radars,
                &self.args,
                Brand::Garmin,
                None,
                Some("B"),
                detected_type.pixel_values(),
                detected_type.spokes_per_revolution(),
                detected_type.max_spoke_len(),
                *from,
                nic_addr.clone(),
                spoke_data_addr,
                report_addr,
                radar_send,
                |id, tx| settings::new(id, tx, &self.args, detected_type, &capabilities, true),
                has_doppler,
                false,
            );
            if has_doppler {
                info_b.set_doppler_levels(DOPPLER_LEVELS_PER_DIRECTION);
            }
            let name_b = format!("{} B", model_name);
            let user_b = format!("{} B", user_name);
            info_b.controls.set_model_name(name_b);
            info_b.controls.set_user_name(user_b);
            info_b.set_ranges(ranges);
            Some(info_b)
        } else {
            None
        };

        self.state
            .lock()
            .unwrap()
            .radars
            .insert(*from, RadarState::Registered);
        self.found(radar_info, info_b, radars, subsys);
    }
}

/// Build the hardcoded HD range table. HD radars do not broadcast a
/// range table — the MFD uses a fixed list. Mayara
/// publishes the union of the metric and nautical fractions used by
/// the GMR 18 HD / GMR 24 HD.
/// Infer a model name from the capability bitmap when neither the CDM
/// product data (0x0392) nor the product_id lookup table gave us a name.
/// Uses the detection heuristic from research/garmin/feature-detection.md.
fn detect_model_name(detected_type: GarminRadarType, caps: &GarminCapabilities) -> String {
    let generation = if detected_type == GarminRadarType::HD {
        "HD"
    } else if caps.has_transmit_channel_select() || caps.has_no_tx_zone_2() {
        // TX channel select (0xb2) or second no-TX zone (0xbf) → Fantom Pro
        "Fantom Pro"
    } else if caps.has_motionscope() || caps.has_doppler_sensitivity() {
        // Doppler mode (0xa3) or sensitivity (0xc3) → Fantom
        "Fantom"
    } else if caps.has_scan_average() {
        // Scan average (0xca) without Doppler → xHD3
        "xHD3"
    } else if caps.has_pulse_expansion() || caps.has_target_size_mode() {
        // Pulse expansion (0xab) or target size (0xbd) → xHD2
        "xHD2"
    } else {
        "xHD"
    };
    format!("Garmin {}", generation)
}

fn hd_ranges() -> Ranges {
    let mut all: Vec<i32> = GARMIN_HD_RANGES_METRIC.to_vec();
    for &r in GARMIN_HD_RANGES_NAUTICAL {
        if !all.contains(&r) {
            all.push(r);
        }
    }
    Ranges::new_by_distance(&all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_radar_type_recognizes_hd_packets() {
        assert_eq!(
            GarminLocator::detect_radar_type(MSG_HD_SPOKE),
            Some(GarminRadarType::HD)
        );
        assert_eq!(
            GarminLocator::detect_radar_type(MSG_HD_STATE),
            Some(GarminRadarType::HD)
        );
        assert_eq!(
            GarminLocator::detect_radar_type(MSG_HD_SETTINGS),
            Some(GarminRadarType::HD)
        );
    }

    #[test]
    fn detect_radar_type_recognizes_xhd_packets() {
        assert_eq!(
            GarminLocator::detect_radar_type(MSG_RANGE_A),
            Some(GarminRadarType::XHD)
        );
        assert_eq!(
            GarminLocator::detect_radar_type(MSG_TRANSMIT_MODE),
            Some(GarminRadarType::XHD)
        );
        assert_eq!(
            GarminLocator::detect_radar_type(MSG_SCANNER_STATE),
            Some(GarminRadarType::XHD)
        );
    }

    #[test]
    fn detect_radar_type_returns_none_for_unrelated_packets() {
        // The CDM heartbeat is handled separately, not via detect_radar_type.
        assert_eq!(GarminLocator::detect_radar_type(MSG_CDM_HEARTBEAT), None);
        assert_eq!(GarminLocator::detect_radar_type(0xdeadbeef), None);
    }

    #[test]
    fn hd_ranges_contains_metric_and_nautical_fractions() {
        let ranges = hd_ranges();
        let distances: Vec<i32> = ranges.all.iter().map(|r| r.distance()).collect();
        // Smallest is the 232 m (1/8 NM) entry shared by both tables.
        assert!(distances.contains(&232));
        // Both 250 m (metric) and 463 m (1/4 NM) appear, exercising the
        // dedupe path.
        assert!(distances.contains(&250));
        assert!(distances.contains(&463));
        // The largest entry is 88896 m (48 NM) — well within i32.
        assert!(distances.contains(&88896));
        // Ranges are sorted and deduped by new_by_distance.
        for w in distances.windows(2) {
            assert!(w[0] < w[1], "ranges should be sorted and deduped");
        }
    }

    #[test]
    fn radar_type_supported_only_for_hd_and_xhd() {
        assert!(GarminRadarType::HD.is_supported());
        assert!(GarminRadarType::XHD.is_supported());
        assert!(!GarminRadarType::XHD2.is_supported());
        assert!(!GarminRadarType::XHD3.is_supported());
        assert!(!GarminRadarType::Fantom.is_supported());
    }

    #[test]
    fn detect_model_name_from_capabilities() {
        use super::capabilities::cap;

        // Plain xHD — no advanced bits
        let mut caps = GarminCapabilities::empty();
        caps.set(cap::TRANSMIT_MODE);
        assert_eq!(
            detect_model_name(GarminRadarType::XHD, &caps),
            "Garmin xHD"
        );

        // xHD2 — has pulse expansion
        caps.set(cap::PULSE_EXPANSION_A);
        assert_eq!(
            detect_model_name(GarminRadarType::XHD, &caps),
            "Garmin xHD2"
        );

        // Fantom — has Doppler
        caps.set(cap::DOPPLER_RANGE_A);
        assert_eq!(
            detect_model_name(GarminRadarType::XHD, &caps),
            "Garmin Fantom"
        );

        // Fantom Pro — has TX channel select
        caps.set(cap::TRANSMIT_CHANNEL_MODE);
        assert_eq!(
            detect_model_name(GarminRadarType::XHD, &caps),
            "Garmin Fantom Pro"
        );

        // HD is always HD
        assert_eq!(
            detect_model_name(GarminRadarType::HD, &caps),
            "Garmin HD"
        );
    }
}

impl RadarLocator for GarminLocator {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        self.process_report(message, from, nic_addr, radars, subsys)
    }

    fn clone(&self) -> Box<dyn RadarLocator> {
        Box::new(Clone::clone(self))
    }
}

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if addresses.iter().any(|i| i.id == LocatorId::Garmin) {
        return;
    }

    // Build a single GarminLocator and clone it into both LocatorAddress
    // entries so the CDM listener and the report listener share the same
    // backing maps (radars + product_ids). The Arc<Mutex<...>> inside
    // makes the clones cheap and gives them a single source of truth.
    let locator = GarminLocator::new(args.clone());

    addresses.push(LocatorAddress::new(
        LocatorId::Garmin,
        &REPORT_ADDRESS,
        Brand::Garmin,
        vec![],
        Box::new(Clone::clone(&locator)),
    ));

    addresses.push(LocatorAddress::new(
        LocatorId::GarminCdm,
        &CDM_HEARTBEAT_ADDRESS,
        Brand::Garmin,
        vec![],
        Box::new(locator),
    ));
}
