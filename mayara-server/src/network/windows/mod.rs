// deprecated_marked_for_delete: This entire module is dead code - only used by legacy locator
// which has been removed. Will be deleted in a future cleanup.

/*
extern crate windows;

use std::ptr::null_mut;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use w32_error::W32Error;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_SERVICE_NOT_ACTIVE, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::NetworkManagement::IpHelper::NotifyAddrChange;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::OVERLAPPED;

use crate::radar::RadarError;

pub async fn wait_for_ip_addr_change(cancel_token: CancellationToken) -> Result<(), RadarError> {
    let (tx, mut rx) = mpsc::channel(1);

    let handle = unsafe {
        // Spawn a blocking task to wait for the event
        let handle = tokio::task::spawn_blocking(move || match create_ip_addr_change_event() {
            Ok(event) => {
                log::debug!("IP address change event created");
                let result = WaitForSingleObject(event, INFINITE);
                match result {
                    WAIT_OBJECT_0 => {
                        log::debug!("IP address change event handled");
                        let _ = tx.send(Ok(()));
                    }
                    _ => {
                        let windows_error = W32Error::last_thread_error();
                        log::error!(
                            "IP address change event failed with error: {}",
                            windows_error
                        );
                        let _ = tx.send(Err(RadarError::OSError(windows_error.to_string())));
                    }
                }
                let _ = CloseHandle(event);
            }
            Err(e) => {
                log::error!("Failed to create IP address change event: {}", e);
                let _ = tx.send(Err(e));
            }
        });
        handle
    };

    // Wait asynchronously for a change or cancellation
    tokio::select! {
        _ = cancel_token.cancelled() => {
            log::warn!("IP address change monitoring cancelling");
            // Cancel the operation
            handle.abort();
            log::warn!("IP address change monitoring cancelled");
            return Err(RadarError::Shutdown);
        }
        result = rx.recv() => {
            log::error!("Received result: {:?}", result);
            match result {
                Some(Ok(())) | None => {
                    return Ok(());
                }
                Some(Err(e)) => {
                    return Err(e);
                }
            }
        }
    }
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
        let mut overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };

        // Register for address change notifications
        let notify_result = NotifyAddrChange(null_mut(), &mut overlapped);
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
        WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle,
        WLAN_INTERFACE_INFO_LIST,
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
*/
