use log::{debug, trace};

// Use mayara-core for report parsing (pure, WASM-compatible)
use mayara_core::protocol::garmin::{parse_report, Report};

pub fn process(report: &[u8]) {
    match parse_report(report) {
        Ok(r) => match r {
            Report::ScanSpeed(v) => debug!("Scan speed {}", v),
            Report::TransmitState(state) => debug!("Transmit state {:?}", state),
            Report::Range(m) => debug!("Range {} m", m),
            Report::Gain { mode, value, level } => {
                debug!("Gain mode={:?} value={} level={:?}", mode, value, level);
            }
            Report::BearingAlignment(deg) => debug!("Bearing alignment {:.1}", deg),
            Report::CrosstalkRejection(v) => debug!("Crosstalk rejection {}", v),
            Report::RainClutter { mode, level } => {
                debug!("Rain clutter mode={} level={}", mode, level);
            }
            Report::SeaClutter {
                mode,
                level,
                auto_level,
            } => {
                debug!(
                    "Sea clutter mode={} level={} auto_level={}",
                    mode, level, auto_level
                );
            }
            Report::NoTransmitZone {
                mode,
                start_deg,
                end_deg,
            } => {
                debug!(
                    "No transmit zone mode={} start={:.1} end={:.1}",
                    mode, start_deg, end_deg
                );
            }
            Report::TimedIdle {
                mode,
                time,
                run_time,
            } => {
                debug!(
                    "Timed idle mode={} time={} run_time={}",
                    mode, time, run_time
                );
            }
            Report::ScannerStatus {
                status,
                change_in_ms,
            } => {
                if change_in_ms > 0 {
                    debug!("Scanner status change in {} ms", change_in_ms);
                } else {
                    debug!("Scanner status {}", status);
                }
            }
            Report::ScannerMessage(msg) => debug!("Scanner message \"{}\"", msg),
            Report::Unknown {
                packet_type,
                value,
                raw,
            } => {
                trace!(
                    "0x{:04X}: value 0x{:X} / {} len {}",
                    packet_type,
                    value,
                    value,
                    raw.len()
                );
            }
        },
        Err(e) => {
            trace!("Failed to parse Garmin report: {}", e);
        }
    }
}
