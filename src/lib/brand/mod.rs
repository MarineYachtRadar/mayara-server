use std::collections::HashSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};

use async_trait::async_trait;
use serde::Serialize;
use tokio_graceful_shutdown::SubsystemHandle;

#[cfg(feature = "emulator")]
pub(crate) mod emulator;
#[cfg(feature = "furuno")]
pub(crate) mod furuno;
#[cfg(feature = "garmin")]
pub(crate) mod garmin;
#[cfg(feature = "koden")]
pub(crate) mod koden;
#[cfg(feature = "navico")]
pub(crate) mod navico;
#[cfg(feature = "raymarine")]
pub(crate) mod raymarine;

use crate::locator::LocatorAddress;
use crate::radar::settings::{ControlValue, SharedControls};
use crate::radar::{RadarError, SharedRadars};
use crate::{Brand, Cli};

#[derive(PartialEq, Eq, Copy, Clone, Serialize, Debug)]
pub(crate) enum LocatorId {
    GenBR24,
    Gen3Plus,
    Furuno,
    Garmin,
    GarminCdm,
    Koden,
    Raymarine,
}

pub(crate) fn create_brand_listeners(
    listen_addresses: &mut Vec<LocatorAddress>,
    brands: &mut HashSet<Brand>,
    args: &Cli,
) {
    // When emulator mode is enabled, skip all real radar brands
    #[cfg(feature = "emulator")]
    if args.emulator {
        emulator::new(args, listen_addresses);
        brands.insert(Brand::Emulator);
        return;
    }

    #[cfg(feature = "navico")]
    if args.brand.unwrap_or(Brand::Navico) == Brand::Navico {
        navico::new(args, listen_addresses);
        brands.insert(Brand::Navico);
    }
    #[cfg(feature = "furuno")]
    if args.brand.unwrap_or(Brand::Furuno) == Brand::Furuno {
        furuno::new(args, listen_addresses);
        brands.insert(Brand::Furuno);
    }
    #[cfg(feature = "raymarine")]
    if args.brand.unwrap_or(Brand::Raymarine) == Brand::Raymarine {
        raymarine::new(args, listen_addresses);
        brands.insert(Brand::Raymarine);
    }
    #[cfg(feature = "garmin")]
    if args.brand.unwrap_or(Brand::Garmin) == Brand::Garmin {
        garmin::new(args, listen_addresses);
        brands.insert(Brand::Garmin);
    }
    #[cfg(feature = "koden")]
    if args.brand.unwrap_or(Brand::Koden) == Brand::Koden {
        koden::new(args, listen_addresses);
        brands.insert(Brand::Koden);
    }
}

///
/// All brand specific code should implement the following traits, in order to be complete
///

///
/// Every brand must try to create a self-organizing locator of any radars.
/// It receives information about the ethernet card and any radars already found.
///
pub(crate) trait RadarLocator: Send {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error>;

    fn clone(&self) -> Box<dyn RadarLocator>;
}

///
/// Every brand should be able to send commands to the radar using the CommandSender trait.
///
#[async_trait]
pub trait CommandSender {
    ///
    /// Apply a control value to a specific control
    ///
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError>;
}
