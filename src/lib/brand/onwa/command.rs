use async_trait::async_trait;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio::net::UdpSocket;

use super::protocol::*;
use crate::brand::CommandSender;
use crate::network::create_connected_send;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError, RadarInfo};

pub(crate) struct Command {
    key: String,
    radar_addr: SocketAddrV4,
    nic_addr: Ipv4Addr,
    socket: Option<UdpSocket>,
    timer: u32,
}

impl Command {
    pub(crate) fn new(info: &RadarInfo) -> Self {
        Command {
            key: info.key(),
            radar_addr: info.send_command_addr,
            nic_addr: info.nic_addr,
            socket: None,
            timer: 0,
        }
    }

    async fn send(&mut self, packet: &[u8]) -> Result<(), RadarError> {
        if self.socket.is_none() {
            let socket =
                create_connected_send(&self.radar_addr, &self.nic_addr).map_err(RadarError::Io)?;
            self.socket = Some(socket);
        }
        log::trace!("{}: sending {}", self.key, String::from_utf8_lossy(packet));
        let socket = self.socket.as_ref().expect("socket was just created");
        if let Err(e) = socket.send(packet).await {
            // A fresh socket is tried on the next send, in case the
            // interface came back with a different setup.
            self.socket = None;
            return Err(RadarError::Io(e));
        }
        Ok(())
    }

    async fn send_all(&mut self, packets: &[Vec<u8>]) -> Result<(), RadarError> {
        for packet in packets {
            self.send(packet).await?;
        }
        Ok(())
    }

    /// What the chartplotter sends every second.
    pub(crate) async fn send_keepalive(&mut self) -> Result<(), RadarError> {
        self.timer = self.timer.wrapping_add(1);
        self.send_all(&keepalive_commands(self.timer)).await
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        match cv.id {
            ControlId::Power => {
                let transmit = cv.as_i32()? == Power::Transmit as u32 as i32;
                // The K-ASTRAL refuses to transmit until its countdown ends,
                // and so does Mayara; whether the radar would refuse is
                // untested.
                let warming_up = controls
                    .get(&ControlId::Power)
                    .and_then(|c| c.value)
                    .is_some_and(|p| p as u32 == Power::Preparing as u32);
                if transmit && warming_up {
                    return Err(RadarError::WarmingUp);
                }
                self.send_all(&power_commands(transmit)).await
            }
            ControlId::Range => {
                let current = controls
                    .get(&ControlId::Range)
                    .and_then(|c| c.value)
                    .map_or(0, |m| range_index_for(m as i32));
                let wanted = range_index_for(cv.as_i32()?);
                self.send_all(&range_commands(current, wanted)).await
            }
            ControlId::Gain => self.send(&gain_command(cv.as_f64()?)).await,
            ControlId::Sea => self.send(&sea_command(cv.as_f64()?)).await,
            ControlId::Rain => self.send(&rain_command(cv.as_f64()?)).await,
            ControlId::InterferenceRejection => {
                self.send(&command("IRLVL", &cv.as_i32()?.to_string()))
                    .await
            }
            ControlId::TargetExpansion => {
                self.send(&command("ESLVL", &cv.as_i32()?.to_string()))
                    .await
            }
            ControlId::NoiseRejection => {
                self.send(&command("NSELM", &cv.as_i32()?.to_string()))
                    .await
            }
            ControlId::DisplayTiming => {
                self.send(&command("SWPTM", &cv.as_i32()?.to_string()))
                    .await
            }
            ControlId::NoTransmitSector1 => {
                if let (Ok(start), Ok(end)) = (cv.as_f64(), cv.end_as_f64()) {
                    self.send(&dead_sector_command(start, end)).await?;
                }
                match cv.enabled {
                    Some(enabled) => {
                        self.send(&command("DEASW", if enabled { "1" } else { "0" }))
                            .await
                    }
                    None => Ok(()),
                }
            }
            _ => Err(RadarError::CannotSetControlId(cv.id)),
        }
    }
}
