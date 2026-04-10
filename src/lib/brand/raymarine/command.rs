use async_trait::async_trait;
use tokio::net::UdpSocket;

use super::BaseModel;
use crate::brand::CommandSender;
use crate::network::create_multicast_send;
use crate::radar::range::Ranges;
use crate::radar::settings::{ControlValue, SharedControls};
use crate::radar::{RadarError, RadarInfo};

mod quantum;
mod rd;

pub struct Command {
    key: String,
    info: RadarInfo,
    model: BaseModel,
    sock: Option<UdpSocket>,
}

impl Command {
    pub fn new(info: RadarInfo, model: BaseModel) -> Self {
        Command {
            key: info.key(),
            info,
            model,
            sock: None,
        }
    }

    pub fn set_ranges(&mut self, ranges: Ranges) {
        self.info.ranges = ranges;
    }

    async fn start_socket(&mut self) -> Result<(), RadarError> {
        match create_multicast_send(&self.info.send_command_addr, &self.info.nic_addr) {
            Ok(sock) => {
                log::debug!(
                    "{} {} via {}: sending commands",
                    self.key,
                    &self.info.send_command_addr,
                    &self.info.nic_addr
                );
                self.sock = Some(sock);

                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} {} via {}: create multicast failed: {}",
                    self.key,
                    &self.info.send_command_addr,
                    &self.info.nic_addr,
                    e
                );
                Err(RadarError::Io(e))
            }
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> Result<(), RadarError> {
        if self.sock.is_none() {
            self.start_socket().await?;
        }
        if let Some(sock) = &self.sock {
            sock.send(message).await.map_err(RadarError::Io)?;
            log::trace!("{}: sent {:02X?}", self.key, message);
        }

        Ok(())
    }

    /// Send the 1-second keep-alive heartbeat. Without this the radar
    /// drops the connection after 60 seconds.
    pub async fn send_heartbeat(&mut self) -> Result<(), RadarError> {
        use super::protocol::*;
        match self.model {
            BaseModel::Quantum => self.send(&HEARTBEAT_QUANTUM_1S).await,
            BaseModel::RD => self.send(&HEARTBEAT_RD_1S).await,
        }
    }

    /// Send the 5-second extended keep-alive with MARPA/AIS option data.
    pub async fn send_heartbeat_5s(&mut self) -> Result<(), RadarError> {
        use super::protocol::*;
        match self.model {
            BaseModel::Quantum => self.send(&HEARTBEAT_QUANTUM_5S).await,
            BaseModel::RD => self.send(&HEARTBEAT_RD_5S).await,
        }
    }

    fn scale_100_to_byte(a: f64) -> u8 {
        // Map range 0..100 to 0..255
        let mut r = a * 255.0 / 100.0;
        if r > 255.0 {
            r = 255.0;
        } else if r < 0.0 {
            r = 0.0;
        }
        r as u8
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        let value = cv.as_f64()?;

        match self.model {
            BaseModel::RD => rd::set_control(self, cv, value, controls).await,
            BaseModel::Quantum => quantum::set_control(self, cv, value, controls).await,
        }
    }
}
