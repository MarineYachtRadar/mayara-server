use async_trait::async_trait;
use std::sync::Arc;
use tokio::net::UdpSocket;

use super::BaseModel;
use crate::brand::CommandSender;
use crate::network::create_multicast_send;
use crate::radar::range::Ranges;
use crate::radar::settings::{ControlValue, SharedControls};
use crate::radar::{RadarError, RadarInfo};

mod quantum;
mod rd;

pub(crate) struct Command {
    key: String,
    info: RadarInfo,
    model: BaseModel,
    sock: Option<Arc<UdpSocket>>,
}

impl Command {
    pub(crate) fn new(info: RadarInfo, model: BaseModel) -> Self {
        Command {
            key: info.key(),
            info,
            model,
            sock: None,
        }
    }

    /// Use a caller-provided, already-connected socket for sending instead
    /// of opening our own. Used by the unicast-stream topology so commands
    /// and the radar's replies share one socket (and one source port).
    pub(crate) fn with_shared_socket(mut self, sock: Arc<UdpSocket>) -> Self {
        self.sock = Some(sock);
        self
    }

    pub(crate) fn set_ranges(&mut self, ranges: Ranges) {
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
                self.sock = Some(Arc::new(sock));

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
        let r = (a * 255.0 / 100.0).clamp(0.0, 255.0);
        r.round() as u8
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
