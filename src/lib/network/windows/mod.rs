extern crate windows;

use std::ptr::null_mut;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use w32_error::W32Error;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::NetworkManagement::IpHelper::{CancelIPChangeNotify, NotifyAddrChange};
use windows::Win32::NetworkManagement::Ndis::{
    NDIS_PHYSICAL_MEDIUM, NdisPhysicalMediumBluetooth, NdisPhysicalMediumNative802_11,
    NdisPhysicalMediumWirelessLan, NdisPhysicalMediumWirelessWan,
};
use windows::Win32::System::IO::OVERLAPPED;
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};

use crate::network::LinkKind;
use crate::radar::RadarError;

/// Create a manual‑reset, initially non‑signaled event.
fn new_manual_event() -> Result<HANDLE, std::io::Error> {
    let handle = unsafe { CreateEventW(None, true, false, None)? };
    Ok(handle)
}

/// Signal the given event.  Returns `Err` if the call fails.
fn signal_event(event: HANDLE) -> Result<(), std::io::Error> {
    unsafe {
        SetEvent(event)?;
        Ok(())
    }
}

/// Listens on the channel and signals `h_chan` whenever a message arrives.
async fn bridge_channel_to_event(
    cancel_token: CancellationToken,
    tx_ip_change: broadcast::Sender<()>,
) {
    let cancel_handle = new_manual_event().unwrap().0 as usize;

    tokio::task::spawn_blocking(move || wait_for_ip_addr_change(cancel_handle, tx_ip_change));

    cancel_token.cancelled().await;
    // We ignore the payload – we just need to wake up the Windows wait.
    let cancel_handle = HANDLE(cancel_handle as *mut core::ffi::c_void);

    if let Err(e) = signal_event(cancel_handle) {
        log::error!("Failed to signal event from channel: {}", e);
    }
}

/// Report every change to the local IP address mapping on `tx_ip_change` until
/// `cancel_token` is cancelled.
pub async fn spawn_wait_for_ip_addr_change(
    cancel_token: CancellationToken,
    tx_ip_change: broadcast::Sender<()>,
) {
    tokio::task::spawn(bridge_channel_to_event(cancel_token, tx_ip_change));
}

/// Block on address changes, forwarding each to `tx_ip_change`, until the event
/// behind `cancel_handle` is signaled.
///
/// `HANDLE` is not `Send`, so the cancellation event is passed as a bare address
/// and rebuilt here; the caller owns it and closes it.
fn wait_for_ip_addr_change(
    cancel_handle: usize,
    tx_ip_change: broadcast::Sender<()>,
) -> Result<(), RadarError> {
    let cancel_handle = HANDLE(cancel_handle as *mut core::ffi::c_void);

    let event = match new_manual_event() {
        Ok(event) => event,
        Err(e) => {
            log::error!("Failed to create IP address change event: {}", e);
            return Ok(());
        }
    };

    // The I/O manager writes the completion status into this structure when an
    // address changes, so it has to outlive every notification armed against it
    // and the pending request has to be cancelled before it goes out of scope.
    let mut overlapped = OVERLAPPED {
        hEvent: event,
        ..Default::default()
    };

    log::debug!("IP address change event created");
    loop {
        if let Err(e) = arm_ip_addr_change_notification(&mut overlapped) {
            log::error!("Failed to register for IP address changes: {}", e);
            break;
        }

        // The wait reports the handle that woke it as `WAIT_OBJECT_0` plus its
        // index in the array below.
        const ADDRESS_CHANGED: u32 = WAIT_OBJECT_0.0;
        const CANCELLED: u32 = WAIT_OBJECT_0.0 + 1;

        let result = unsafe { WaitForMultipleObjects(&[event, cancel_handle], false, INFINITE) };
        match result.0 {
            ADDRESS_CHANGED => {
                log::debug!("IP address change event handled");
                let _ = tx_ip_change.send(());
            }
            CANCELLED => {
                break;
            }
            _ => {
                let windows_error = W32Error::last_thread_error();
                log::error!(
                    "IP address change event failed with error: {}",
                    windows_error
                );
                break;
            }
        }
    }

    unsafe {
        // The last notification armed above is still pending.
        let _ = CancelIPChangeNotify(&overlapped);
        let _ = CloseHandle(event);
    }
    Ok(())
}

/// Arm a single address change notification against `overlapped`.
///
/// `NotifyAddrChange` fires once per registration, so a caller wanting more than
/// the first change re-arms after every notification.
fn arm_ip_addr_change_notification(overlapped: &mut OVERLAPPED) -> Result<(), RadarError> {
    // The event is manual-reset and stays signaled once a notification arrives.
    // The I/O manager also resets it when the request starts, but resetting here
    // keeps a stale signal from turning the wait into a spin.
    if let Err(e) = unsafe { ResetEvent(overlapped.hEvent) } {
        log::error!("Failed to reset IP address change event: {}", e);
        return Err(RadarError::OSError(e.to_string()));
    }

    // Kept at the provenance of the mutable borrow: the kernel writes here while
    // the request is pending, even though the binding takes a const pointer.
    let notify_result = unsafe { NotifyAddrChange(null_mut(), &raw const *overlapped) };
    if notify_result != ERROR_SUCCESS.0 && notify_result != ERROR_IO_PENDING.0 {
        let windows_error = W32Error::new(notify_result);
        log::error!(
            "NotifyAddrChange failed with error: {}: {}",
            notify_result,
            windows_error
        );
        return Err(RadarError::OSError(windows_error.to_string()));
    }
    Ok(())
}

/// Classify an interface by the link technology behind it.
///
/// `interface_name` is an adapter *friendly* name, the same string that
/// `NetworkInterface::show()` reports. `MIB_IF_ROW2` is the only interface table
/// carrying the friendly name (`Alias`), the adapter type and the physical
/// medium together, and all three are needed: Windows reports a Bluetooth
/// personal area network as `IF_TYPE_ETHERNET_CSMACD`, exactly like real
/// Ethernet, and only `PhysicalMediumType` tells them apart.
///
/// An interface missing from the table is reported as [`LinkKind::Wired`] so a
/// lookup failure never silently hides a radar.
pub fn link_kind(interface_name: &str) -> LinkKind {
    lookup_link_kind(interface_name).unwrap_or(LinkKind::Wired)
}

fn lookup_link_kind(interface_name: &str) -> Option<LinkKind> {
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};

    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = null_mut();
        let result = GetIfTable2(&mut table);
        if result != ERROR_SUCCESS {
            log::warn!(
                "GetIfTable2 failed with error: {}, treating '{}' as wired",
                W32Error::new(result.0),
                interface_name
            );
            return None;
        }

        // `Table` is a C flexible array: its one-element head is followed by
        // `NumEntries` rows in the same allocation.
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let kind = rows
            .iter()
            .find(|row| nul_terminated(&row.Alias) == interface_name)
            .map(|row| classify(row.Type, row.PhysicalMediumType));

        FreeMibTable(table as _);
        kind
    }
}

fn classify(if_type: u32, medium: NDIS_PHYSICAL_MEDIUM) -> LinkKind {
    use windows::Win32::NetworkManagement::IpHelper::{IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211};

    // Checked before the adapter type, because a Bluetooth PAN claims to be
    // Ethernet and would otherwise pass as a usable wired link.
    if medium == NdisPhysicalMediumBluetooth {
        return LinkKind::Unusable;
    }

    let wireless_medium = medium == NdisPhysicalMediumNative802_11
        || medium == NdisPhysicalMediumWirelessLan
        || medium == NdisPhysicalMediumWirelessWan;

    match if_type {
        IF_TYPE_IEEE80211 => LinkKind::Wireless,
        IF_TYPE_ETHERNET_CSMACD if wireless_medium => LinkKind::Wireless,
        IF_TYPE_ETHERNET_CSMACD => LinkKind::Wired,
        // Loopback, tunnels (Teredo, 6to4, IP-HTTPS), PPP/VPN dial-up, cellular
        // modems and the rest cannot carry a radar's multicast spoke stream.
        _ => LinkKind::Unusable,
    }
}

/// Decode a fixed-size, NUL-padded Windows UTF-16 string.
fn nul_terminated(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::IpHelper::{
        IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IF_TYPE_PPP, IF_TYPE_SOFTWARE_LOOPBACK,
        IF_TYPE_TUNNEL,
    };
    use windows::Win32::NetworkManagement::Ndis::{
        NdisPhysicalMedium802_3, NdisPhysicalMediumUnspecified,
    };

    #[test]
    fn ethernet_is_wired() {
        assert_eq!(
            classify(IF_TYPE_ETHERNET_CSMACD, NdisPhysicalMedium802_3),
            LinkKind::Wired
        );
    }

    #[test]
    fn native_80211_is_wireless() {
        assert_eq!(
            classify(IF_TYPE_IEEE80211, NdisPhysicalMediumNative802_11),
            LinkKind::Wireless
        );
    }

    #[test]
    fn bluetooth_pan_is_unusable() {
        // Values observed on a real adapter: Windows types a Bluetooth PAN as
        // Ethernet, so only the medium keeps it out of radar discovery.
        assert_eq!(
            classify(IF_TYPE_ETHERNET_CSMACD, NdisPhysicalMediumBluetooth),
            LinkKind::Unusable
        );
    }

    #[test]
    fn tunnels_loopback_and_ppp_are_unusable() {
        for if_type in [IF_TYPE_TUNNEL, IF_TYPE_PPP, IF_TYPE_SOFTWARE_LOOPBACK] {
            assert_eq!(
                classify(if_type, NdisPhysicalMediumUnspecified),
                LinkKind::Unusable,
                "if_type {} must be unusable",
                if_type
            );
        }
    }

    #[test]
    fn alias_decoding_stops_at_the_nul_padding() {
        let mut alias = [0u16; 257];
        for (slot, c) in alias.iter_mut().zip("WiFi".encode_utf16()) {
            *slot = c;
        }
        assert_eq!(nul_terminated(&alias), "WiFi");
    }

    #[test]
    fn an_unknown_interface_is_assumed_wired() {
        assert_eq!(link_kind("no such interface"), LinkKind::Wired);
    }

    #[test]
    fn the_address_change_watcher_stops_when_cancelled() {
        use std::sync::mpsc;
        use std::time::Duration;

        let cancel = new_manual_event().expect("cancel event");
        let (tx_ip_change, _rx) = broadcast::channel(1);
        let (done, cancelled) = mpsc::channel();

        // `HANDLE` is not `Send`, so the watcher takes it as a bare address.
        let cancel_handle = cancel.0 as usize;
        std::thread::spawn(move || {
            let _ = done.send(wait_for_ip_addr_change(cancel_handle, tx_ip_change).is_ok());
        });

        signal_event(cancel).expect("signal cancel event");

        assert_eq!(
            cancelled.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the watcher must return once its cancel event is signaled"
        );
        unsafe {
            let _ = CloseHandle(cancel);
        }
    }
}
