// deprecated_marked_for_delete: This entire module is dead code - only used by legacy locator
// which has been removed. Will be deleted in a future cleanup.

/*
use std::ptr;

use core_foundation::runloop::{
    kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopAddSource, CFRunLoopStop,
};
use system_configuration::core_foundation::array::CFArray;
use system_configuration::core_foundation::base::TCFType;
use system_configuration::core_foundation::string::CFString;
use system_configuration::dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder};
use system_configuration::sys::dynamic_store::SCDynamicStoreCreateRunLoopSource;
use tokio_util::sync::CancellationToken;

use crate::radar::RadarError;

pub(crate) async fn wait_for_ip_addr_change(
    cancellation_token: CancellationToken,
) -> Result<(), RadarError> {
    // Create a dynamic store session
    let store: SCDynamicStore = SCDynamicStoreBuilder::new("IPChangeMonitor").build();

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
            core_foundation::runloop::CFRunLoopRunResult::Finished => {
                log::trace!("CFRunLoop finished.");
            }
            core_foundation::runloop::CFRunLoopRunResult::Stopped => {
                log::trace!("CFRunLoop stopped.");
                break;
            }
            core_foundation::runloop::CFRunLoopRunResult::TimedOut => {}
            core_foundation::runloop::CFRunLoopRunResult::HandledSource => {
                log::trace!("CFRunLoop handled source.");
                log::debug!("IP address changed.");
                break;
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

    let store = SCDynamicStoreBuilder::new("networkInterfaceInfo").build();

    let key = format!("State:/Network/Interface/{}/AirPort", interface_name);
    if let Some(_) = store.get(key.as_str()) {
        return true;
    }
    false
}
*/
