use std::ptr;

use system_configuration::core_foundation::array::CFArray;
use system_configuration::core_foundation::base::TCFType;
use system_configuration::core_foundation::runloop::{
    CFRunLoop, CFRunLoopAddSource, CFRunLoopRunResult, CFRunLoopStop, kCFRunLoopDefaultMode,
};
use system_configuration::core_foundation::string::CFString;
use system_configuration::dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder};
use system_configuration::sys::dynamic_store::SCDynamicStoreCreateRunLoopSource;
use tokio::sync::broadcast::Sender;
use tokio_util::sync::CancellationToken;

use crate::radar::RadarError;

pub async fn spawn_wait_for_ip_addr_change(
    cancel_token: CancellationToken,
    tx_ip_change: Sender<()>,
) {
    tokio::task::spawn_blocking(move || wait_for_ip_addr_change(cancel_token, tx_ip_change));
}

fn wait_for_ip_addr_change(
    cancellation_token: CancellationToken,
    tx_ip_change: Sender<()>,
) -> Result<(), RadarError> {
    // Create a dynamic store session
    let store: SCDynamicStore = SCDynamicStoreBuilder::new("IPChangeMonitor")
        .build()
        .expect("Failed to create SCDynamicStore session for IP change monitor");

    // Define the key to monitor for changes (IPv4 addresses)
    let watched_keys = &CFArray::from_CFTypes(&vec![CFString::new("State:/Network/Global/IPv4")]);
    let patterns = &CFArray::from_CFTypes(&vec![CFString::new("State:/Network/Interface/.*/IPv4")]);

    // Set the notification keys
    if !store.set_notification_keys(watched_keys, patterns) {
        panic!("Failed to set notification keys.");
    }

    // Get the current CFRunLoop reference
    let run_loop = CFRunLoop::get_current();

    // Create a run loop source
    let run_loop_source =
        unsafe { SCDynamicStoreCreateRunLoopSource(ptr::null(), store.as_concrete_TypeRef(), 0) };
    if run_loop_source.is_null() {
        panic!("Failed to create run loop source");
    }

    // Add the run loop source to the run loop
    unsafe {
        CFRunLoopAddSource(
            CFRunLoop::get_current().as_concrete_TypeRef(),
            run_loop_source,
            kCFRunLoopDefaultMode,
        )
    };

    loop {
        match CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            std::time::Duration::from_secs(2),
            true,
        ) {
            CFRunLoopRunResult::Finished => {
                log::trace!("CFRunLoop finished.");
            }
            CFRunLoopRunResult::Stopped => {
                log::trace!("CFRunLoop stopped.");
                break;
            }
            CFRunLoopRunResult::TimedOut => {}
            CFRunLoopRunResult::HandledSource => {
                log::trace!("CFRunLoop handled source.");
                log::debug!("IP address changed.");
                let _ = tx_ip_change.send(());
            }
        }
        if cancellation_token.is_cancelled() {
            break;
        }
    }

    // Stop the CFRunLoop from this thread
    unsafe {
        CFRunLoopStop(run_loop.as_concrete_TypeRef());
    }

    Ok(())
}

pub fn is_wireless_interface(interface_name: &str) -> bool {
    use system_configuration::dynamic_store::*;

    let Some(store) = SCDynamicStoreBuilder::new("networkInterfaceInfo").build() else {
        return false;
    };

    let key = format!("State:/Network/Interface/{}/AirPort", interface_name);
    store.get(key.as_str()).is_some()
}
