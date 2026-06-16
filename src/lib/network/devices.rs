//! Per-interface device discovery used by the network-diagnostics export.
//!
//! Combines three best-effort sources (ARP cache, scoped mDNS browse,
//! passive multicast snooping) into one `Vec<Device>` per host NIC where
//! mayara currently has at least one radar locator actively listening.
//! Designed to give a maintainer enough context to answer "is this subnet
//! even populated?" from a downloaded diagnostic file.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;

use serde::Serialize;

use crate::InterfaceApi;
use crate::network::{arp, match_ipv4, mdns_browse, passive_capture};

/// How long the mDNS browse runs. mDNS responders typically reply within
/// 1 s on a quiet LAN; 3 s gives slow MFDs a margin.
const MDNS_DURATION: Duration = Duration::from_secs(3);

/// How long passive multicast capture runs. The shared deadline bounds
/// the whole endpoint's latency; the user clicked a button, so this is
/// also the perceived UI delay.
const CAPTURE_DURATION: Duration = Duration::from_secs(5);

/// A device observed on one host NIC's L2 segment, possibly via multiple
/// evidence sources merged together.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    #[serde(serialize_with = "serialize_ip")]
    pub ip: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    pub hostnames: Vec<String>,
    /// Sources we learned about this device from, e.g.
    /// `"arp"`, `"mdns:_workstation._tcp.local."`, `"passive:ssdp"`.
    pub sources: Vec<String>,
}

/// Run ARP + mDNS + passive capture in parallel and group the merged
/// results by qualifying NIC IPv4 address. NICs that have no active
/// listener (or aren't `Ok`) are skipped — qualification matches the
/// existing diagnostics scope.
///
/// Returns an empty map if no NIC qualifies. In that case the function
/// also skips the network I/O entirely, so the endpoint stays fast on
/// degraded setups (laptop with only Wi-Fi, `--allow-wifi` off, etc.).
pub async fn collect_per_interface(iface_api: &InterfaceApi) -> HashMap<Ipv4Addr, Vec<Device>> {
    let qualifying = collect_qualifying_nics(iface_api);
    if qualifying.is_empty() {
        return HashMap::new();
    }

    let nic_addrs: Vec<Ipv4Addr> = qualifying.iter().map(|q| q.ip).collect();
    let if_names: Vec<String> = {
        let mut names: Vec<String> = qualifying.iter().map(|q| q.name.clone()).collect();
        names.sort();
        names.dedup();
        names
    };

    // ARP is a synchronous one-shot file/process read. Wrap it in
    // spawn_blocking so a slow `arp(8)` on macOS doesn't block the
    // reactor while mDNS + passive capture are running.
    let arp_fut = tokio::task::spawn_blocking(arp::list);
    let mdns_fut = mdns_browse::browse_one_shot(&if_names, MDNS_DURATION);
    let capture_fut = passive_capture::capture_one_shot(&nic_addrs, CAPTURE_DURATION);

    let (arp_res, mdns_hits, passive_hits) = tokio::join!(arp_fut, mdns_fut, capture_fut);
    let arp_entries = arp_res.unwrap_or_else(|e| {
        log::debug!("devices: arp::list join error: {}", e);
        Vec::new()
    });

    let mut per_nic: HashMap<Ipv4Addr, HashMap<Ipv4Addr, Device>> = HashMap::new();
    for q in &qualifying {
        per_nic.insert(q.ip, HashMap::new());
    }

    // 1. ARP: filter to entries that fall in some qualifying NIC's subnet.
    for entry in arp_entries {
        for q in &qualifying {
            if entry.ip != q.ip && match_ipv4(&entry.ip, &q.ip, &q.netmask) {
                let dev = per_nic
                    .get_mut(&q.ip)
                    .expect("seeded above")
                    .entry(entry.ip)
                    .or_insert_with(|| empty_device(entry.ip));
                if dev.mac.is_none() {
                    dev.mac = Some(entry.mac.clone());
                }
                push_unique(&mut dev.sources, "arp".to_string());
                // ARP cache holds each IP once; no need to keep checking.
                break;
            }
        }
    }

    // 2. mDNS: same subnet filter; attach hostname.
    for hit in mdns_hits {
        for q in &qualifying {
            if hit.ip != q.ip && match_ipv4(&hit.ip, &q.ip, &q.netmask) {
                let dev = per_nic
                    .get_mut(&q.ip)
                    .expect("seeded above")
                    .entry(hit.ip)
                    .or_insert_with(|| empty_device(hit.ip));
                push_unique(&mut dev.hostnames, hit.hostname.clone());
                push_unique(&mut dev.sources, format!("mdns:{}", hit.service));
                break;
            }
        }
    }

    // 3. Passive capture: NIC binding is already explicit — no need to
    //    subnet-match; the hit carries the NIC it came in on.
    for hit in passive_hits {
        if let Some(bucket) = per_nic.get_mut(&hit.nic) {
            let dev = bucket.entry(hit.ip).or_insert_with(|| empty_device(hit.ip));
            push_unique(&mut dev.sources, format!("passive:{}", hit.protocol));
        }
    }

    per_nic
        .into_iter()
        .map(|(nic, devs)| {
            let mut list: Vec<Device> = devs.into_values().collect();
            list.sort_by_key(|d| u32::from(d.ip));
            for d in &mut list {
                d.sources.sort();
                d.hostnames.sort();
            }
            (nic, list)
        })
        .collect()
}

struct QualifyingNic {
    name: String,
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
}

fn collect_qualifying_nics(iface_api: &InterfaceApi) -> Vec<QualifyingNic> {
    let mut out = Vec::new();
    for (id, iface) in &iface_api.interfaces {
        let (Some(ip), Some(netmask)) = (iface.ip, iface.netmask) else {
            continue;
        };
        if !matches!(iface.status, crate::InterfaceStatus::Ok) {
            continue;
        }
        let Some(listeners) = iface.listeners.as_ref() else {
            continue;
        };
        let has_active = listeners
            .values()
            .any(|status| status == "Listening" || status == "Active");
        if !has_active {
            continue;
        }
        out.push(QualifyingNic {
            name: id.name.clone(),
            ip,
            netmask,
        });
    }
    out
}

fn empty_device(ip: Ipv4Addr) -> Device {
    Device {
        ip,
        mac: None,
        hostnames: Vec::new(),
        sources: Vec::new(),
    }
}

fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.iter().any(|existing| existing == &item) {
        v.push(item);
    }
}

fn serialize_ip<S: serde::Serializer>(ip: &Ipv4Addr, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&ip.to_string())
}
