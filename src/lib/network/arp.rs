//! Read the host's IPv4 ARP cache.
//!
//! Used by [`super::devices::collect_per_interface`] to seed the
//! per-interface device list with hosts the kernel has recently exchanged
//! traffic with. ARP only sees devices the host has actually talked to in
//! the last ~10-20 minutes (entries are aged out by the kernel), so it
//! misses silent radars that haven't responded to anything yet — but it's
//! a free snapshot with no network I/O.

use std::net::Ipv4Addr;

#[derive(Debug, Clone)]
pub struct ArpEntry {
    pub ip: Ipv4Addr,
    /// Lowercase colon-separated MAC, e.g. `aa:bb:cc:dd:ee:ff`. Incomplete
    /// entries (no resolved MAC yet) are filtered out by the per-platform
    /// readers, so this is always populated.
    pub mac: String,
    pub ifname: Option<String>,
}

/// Read the host ARP cache. Returns an empty vec rather than an error if
/// the cache cannot be read — this is best-effort diagnostic data, not
/// authoritative.
pub fn list() -> Vec<ArpEntry> {
    #[cfg(target_os = "linux")]
    {
        return linux::list();
    }
    #[cfg(target_os = "macos")]
    {
        unix_arp::list()
    }
    #[cfg(target_os = "windows")]
    {
        return windows_arp::list();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ArpEntry, normalize_mac};
    use std::fs;
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    /// `/proc/net/arp` columns (whitespace-separated, header on line 1):
    ///   `IP address  HW type  Flags  HW address         Mask  Device`
    /// Flags `0x0` means "incomplete" — drop those.
    pub(super) fn list() -> Vec<ArpEntry> {
        let raw = match fs::read_to_string("/proc/net/arp") {
            Ok(s) => s,
            Err(e) => {
                log::debug!("arp: /proc/net/arp not readable: {}", e);
                return Vec::new();
            }
        };
        parse(&raw)
    }

    fn parse(raw: &str) -> Vec<ArpEntry> {
        let mut out = Vec::new();
        for line in raw.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 6 {
                continue;
            }
            let (ip_s, flags, mac_s, ifname) = (cols[0], cols[2], cols[3], cols[5]);
            if flags == "0x0" {
                continue;
            }
            let Ok(ip) = Ipv4Addr::from_str(ip_s) else {
                continue;
            };
            let Some(mac) = normalize_mac(mac_s) else {
                continue;
            };
            out.push(ArpEntry {
                ip,
                mac,
                ifname: Some(ifname.to_string()),
            });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_a_typical_proc_arp() {
            let sample = "IP address       HW type     Flags       HW address            Mask     Device\n\
                          192.168.1.1      0x1         0x2         a4:2b:b0:11:22:33     *        eth0\n\
                          192.168.1.50     0x1         0x0         00:00:00:00:00:00     *        eth0\n\
                          192.168.1.99     0x1         0x2         aa-bb-cc-dd-ee-ff     *        eth0\n";
            let entries = parse(sample);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].ip, Ipv4Addr::new(192, 168, 1, 1));
            assert_eq!(entries[0].mac, "a4:2b:b0:11:22:33");
            assert_eq!(entries[0].ifname.as_deref(), Some("eth0"));
            // The 0x0-flag entry must be skipped (incomplete).
            assert_eq!(entries[1].ip, Ipv4Addr::new(192, 168, 1, 99));
            // Dashes are normalized to colons.
            assert_eq!(entries[1].mac, "aa:bb:cc:dd:ee:ff");
        }
    }
}

#[cfg(target_os = "macos")]
mod unix_arp {
    use super::{ArpEntry, normalize_mac};
    use std::net::Ipv4Addr;
    use std::process::Command;
    use std::str::FromStr;

    pub(super) fn list() -> Vec<ArpEntry> {
        // -a: all, -n: numeric (no DNS lookup, much faster on a marine LAN
        // with no DNS server reachable). Bail quietly if arp(8) is missing
        // or refuses to run — the diagnostics are best-effort.
        let output = match Command::new("/usr/sbin/arp").args(["-an"]).output() {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                log::debug!(
                    "arp: arp -an exited with {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                return Vec::new();
            }
            Err(e) => {
                log::debug!("arp: cannot spawn /usr/sbin/arp: {}", e);
                return Vec::new();
            }
        };
        parse(&String::from_utf8_lossy(&output.stdout))
    }

    /// macOS `arp -an` lines look like:
    ///   ? (10.0.0.1) at a4:2b:b0:11:22:33 on en0 ifscope [ethernet]
    ///   ? (10.0.0.50) at (incomplete) on en0 ifscope [ethernet]
    fn parse(raw: &str) -> Vec<ArpEntry> {
        let mut out = Vec::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            let Some(open) = trimmed.find('(') else {
                continue;
            };
            let Some(close_rel) = trimmed[open + 1..].find(')') else {
                continue;
            };
            let ip_s = &trimmed[open + 1..open + 1 + close_rel];
            let Ok(ip) = Ipv4Addr::from_str(ip_s) else {
                continue;
            };
            let rest = &trimmed[open + close_rel + 2..];
            let Some(at_idx) = rest.find(" at ") else {
                continue;
            };
            let after_at = &rest[at_idx + 4..];
            // After " at ", the next whitespace-delimited token is either
            // a MAC or the literal "(incomplete)".
            let Some(mac_tok) = after_at.split_whitespace().next() else {
                continue;
            };
            if mac_tok.starts_with('(') {
                continue;
            }
            let Some(mac) = normalize_mac(mac_tok) else {
                continue;
            };
            let ifname = after_at
                .split_whitespace()
                .skip_while(|t| *t != "on")
                .nth(1)
                .map(|s| s.to_string());
            out.push(ArpEntry { ip, mac, ifname });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_typical_macos_arp() {
            let sample = "\
? (10.0.0.1) at a4:2b:b0:11:22:33 on en0 ifscope [ethernet]
? (10.0.0.50) at (incomplete) on en0 ifscope [ethernet]
halo.local (192.168.1.50) at aa-bb-cc-dd-ee-ff on en1 ifscope permanent [ethernet]
";
            let entries = parse(sample);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].ip, Ipv4Addr::new(10, 0, 0, 1));
            assert_eq!(entries[0].mac, "a4:2b:b0:11:22:33");
            assert_eq!(entries[0].ifname.as_deref(), Some("en0"));
            assert_eq!(entries[1].ip, Ipv4Addr::new(192, 168, 1, 50));
            assert_eq!(entries[1].mac, "aa:bb:cc:dd:ee:ff");
            assert_eq!(entries[1].ifname.as_deref(), Some("en1"));
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_arp {
    use super::{ArpEntry, normalize_mac};
    use std::net::Ipv4Addr;
    use std::process::Command;
    use std::str::FromStr;

    pub(super) fn list() -> Vec<ArpEntry> {
        let output = match Command::new("arp").args(["-a"]).output() {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                log::debug!(
                    "arp: arp -a exited with {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                return Vec::new();
            }
            Err(e) => {
                log::debug!("arp: cannot spawn arp.exe: {}", e);
                return Vec::new();
            }
        };
        parse(&String::from_utf8_lossy(&output.stdout))
    }

    /// Windows `arp -a` is grouped by NIC. Each interface section starts
    /// with a header like:
    ///   `Interface: 192.168.1.5 --- 0xa`
    /// followed by a column header line and one row per neighbour:
    ///   `  192.168.1.1           aa-bb-cc-dd-ee-ff     dynamic`
    /// We track the current section's NIC IP (used as ifname fallback —
    /// Windows interface name lookup is its own rabbit hole) and emit
    /// dynamic entries with a resolved MAC.
    fn parse(raw: &str) -> Vec<ArpEntry> {
        let mut out = Vec::new();
        let mut current_nic: Option<String> = None;
        for line in raw.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Interface:") {
                current_nic = rest.split_whitespace().next().map(|s| s.to_string());
                continue;
            }
            let cols: Vec<&str> = trimmed.split_whitespace().collect();
            if cols.len() < 3 {
                continue;
            }
            let Ok(ip) = Ipv4Addr::from_str(cols[0]) else {
                continue;
            };
            let Some(mac) = normalize_mac(cols[1]) else {
                continue;
            };
            // Skip multicast/broadcast static rows. "dynamic" entries are
            // the kernel's resolved neighbours.
            if !cols[2].eq_ignore_ascii_case("dynamic") {
                continue;
            }
            out.push(ArpEntry {
                ip,
                mac,
                ifname: current_nic.clone(),
            });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_typical_windows_arp() {
            let sample = "\
Interface: 192.168.1.5 --- 0xa
  Internet Address      Physical Address      Type
  192.168.1.1           aa-bb-cc-dd-ee-ff     dynamic
  192.168.1.255         ff-ff-ff-ff-ff-ff     static
  224.0.0.22            01-00-5e-00-00-16     static

Interface: 10.0.0.7 --- 0xb
  Internet Address      Physical Address      Type
  10.0.0.1              11-22-33-44-55-66     dynamic
";
            let entries = parse(sample);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].ip, Ipv4Addr::new(192, 168, 1, 1));
            assert_eq!(entries[0].mac, "aa:bb:cc:dd:ee:ff");
            assert_eq!(entries[0].ifname.as_deref(), Some("192.168.1.5"));
            assert_eq!(entries[1].ip, Ipv4Addr::new(10, 0, 0, 1));
            assert_eq!(entries[1].ifname.as_deref(), Some("10.0.0.7"));
        }
    }
}

/// Accept MACs in either `aa:bb:cc:dd:ee:ff` or `aa-bb-cc-dd-ee-ff` form
/// (both common across the platforms we read from). Output is always
/// lowercase colon-separated.
fn normalize_mac(raw: &str) -> Option<String> {
    let s: String = raw
        .chars()
        .map(|c| {
            if c == '-' {
                ':'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 || parts.iter().any(|p| p.len() != 2) {
        return None;
    }
    if !parts
        .iter()
        .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    if parts.iter().all(|p| *p == "00") {
        // Suppress all-zero MAC entries — Linux uses this for incomplete
        // entries we already filter on `Flags`, but some macOS rows can
        // be a placeholder too.
        return None;
    }
    if parts.iter().all(|p| *p == "ff") {
        // ff:ff:ff:ff:ff:ff is the static ARP entry every OS keeps for
        // the IPv4 broadcast address — not a real device.
        return None;
    }
    Some(s)
}
