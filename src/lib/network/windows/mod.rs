extern crate windows;

use std::ptr::null_mut;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use w32_error::W32Error;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_SERVICE_NOT_ACTIVE, ERROR_SUCCESS, HANDLE, WAIT_EVENT,
};
use windows::Win32::NetworkManagement::IpHelper::NotifyAddrChange;
use windows::Win32::System::IO::OVERLAPPED;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};

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

pub async fn spawn_wait_for_ip_addr_change(
    cancel_token: CancellationToken,
    tx_ip_change: broadcast::Sender<()>,
) {
    tokio::task::spawn(bridge_channel_to_event(cancel_token, tx_ip_change));
}

fn wait_for_ip_addr_change(
    cancel_handle: usize,
    tx_ip_change: broadcast::Sender<()>,
) -> Result<(), RadarError> {
    let cancel_handle = HANDLE(cancel_handle as *mut core::ffi::c_void);

    match create_ip_addr_change_event() {
        Ok(event) => {
            log::debug!("IP address change event created");
            loop {
                let result =
                    unsafe { WaitForMultipleObjects(&[event, cancel_handle], false, INFINITE) };
                match result {
                    WAIT_EVENT(0) => {
                        log::debug!("IP address change event handled");
                        let _ = tx_ip_change.send(());
                    }
                    WAIT_EVENT(1) => {
                        break;
                    }
                    _ => {
                        let windows_error = W32Error::last_thread_error();
                        log::error!(
                            "IP address change event failed with error: {}",
                            windows_error
                        );
                    }
                }
            }
            let _ = unsafe { CloseHandle(event) };
        }
        Err(e) => {
            log::error!("Failed to create IP address change event: {}", e);
        }
    };
    Ok(())
}

fn create_ip_addr_change_event() -> Result<HANDLE, RadarError> {
    unsafe {
        // Create a manual-reset event
        let event = CreateEventW(None, true, false, None)
            .map_err(|_| RadarError::Io(std::io::Error::last_os_error()))?;
        if event.is_invalid() {
            let windows_error = W32Error::last_thread_error();
            log::error!("CreateEventW failed with error: {}", windows_error);
            return Err(RadarError::OSError(windows_error.to_string()));
        }

        // Prepare an OVERLAPPED structure with the event handle
        let overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };

        // Register for address change notifications
        let notify_result = NotifyAddrChange(null_mut(), &overlapped);
        if notify_result != ERROR_SUCCESS.0 && notify_result != ERROR_IO_PENDING.0 {
            let windows_error = W32Error::new(notify_result);
            log::error!(
                "NotifyAddrChange failed with error: {}: {}",
                notify_result,
                windows_error
            );
            let _ = CloseHandle(event);
            return Err(RadarError::OSError(windows_error.to_string()));
        }

        Ok(event)
    }
}

pub fn is_wireless_interface(interface_name: &str) -> bool {
    use std::ptr::null_mut;
    use windows::Win32::NetworkManagement::WiFi::{
        WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory,
        WlanOpenHandle,
    };

    unsafe {
        // Open WLAN handle
        let mut client_handle: HANDLE = Default::default();
        let mut negotiated_version = 0;
        let wlan_result = WlanOpenHandle(2, None, &mut negotiated_version, &mut client_handle);

        if wlan_result == ERROR_SERVICE_NOT_ACTIVE.0 {
            return false;
        }
        if wlan_result != 0 {
            panic!("WlanOpenHandle failed with error: {}", wlan_result);
        }

        let mut interface_list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
        let wlan_enum_result = WlanEnumInterfaces(client_handle, None, &mut interface_list);

        if wlan_enum_result != 0 {
            WlanCloseHandle(client_handle, None);
            panic!("WlanEnumInterfaces failed with error: {}", wlan_enum_result);
        }

        let interfaces = &*interface_list;

        // Check each WLAN interface
        for i in 0..interfaces.dwNumberOfItems {
            let wlan_interface = &interfaces.InterfaceInfo[i as usize];
            let wlan_interface_name =
                String::from_utf16_lossy(&wlan_interface.strInterfaceDescription);
            if wlan_interface_name.trim() == interface_name.trim() {
                WlanFreeMemory(interface_list as _);
                WlanCloseHandle(client_handle, None);
                return true;
            }
        }

        WlanFreeMemory(interface_list as _);
        WlanCloseHandle(client_handle, None);
    }

    false
}
