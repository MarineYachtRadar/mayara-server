use async_trait::async_trait;
use std::sync::Arc;
use tokio::net::UdpSocket;

use super::BaseModel;
use crate::brand::CommandSender;
use crate::network::create_connected_send;
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
    /// Whether this sender is in the unicast-stream topology, where it must
    /// share the report socket. True means the sender must not open its own
    /// command socket — doing so would bind the same host:port as the report
    /// socket and steal the radar's replies. False is the normal multicast
    /// topology, where this sender owns its command socket.
    unicast_mode: bool,
}

impl Command {
    pub(crate) fn new(info: RadarInfo, model: BaseModel, unicast: bool) -> Self {
        Command {
            key: info.key(),
            info,
            model,
            sock: None,
            unicast_mode: unicast,
        }
    }

    /// Use a caller-provided, already-connected socket for sending instead
    /// of opening our own. Used by the unicast-stream topology so commands
    /// and the radar's replies share one socket (and one source port).
    pub(crate) fn set_shared_socket(&mut self, sock: Arc<UdpSocket>) {
        assert!(self.unicast_mode);
        self.sock = Some(sock);
    }

    pub(crate) fn set_ranges(&mut self, ranges: Ranges) {
        self.info.ranges = ranges;
    }

    async fn start_socket(&mut self) -> Result<(), RadarError> {
        assert!(!self.unicast_mode);
        match create_connected_send(&self.info.send_command_addr, &self.info.nic_addr) {
            Ok(sock) => {
                // The command address is often an Axiom that relays to a WiFi
                // radar; like the wake burst, commands must carry TTL > 1 or
                // the relay drops them (issue #160). Set both the multicast
                // and unicast TTL since send_command_addr may be either.
                if let Err(e) = sock.set_multicast_ttl_v4(super::RAYMARINE_RELAY_TTL) {
                    log::warn!("{}: command socket multicast TTL: {}", self.key, e);
                }
                if let Err(e) = sock.set_ttl(super::RAYMARINE_RELAY_TTL) {
                    log::warn!("{}: command socket TTL: {}", self.key, e);
                }
                log::debug!(
                    "{} {} via {}: sending commands",
                    self.key,
                    self.info.send_command_addr,
                    self.info.nic_addr
                );
                self.sock = Some(Arc::new(sock));

                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} {} via {}: send socket failed: {}",
                    self.key,
                    self.info.send_command_addr,
                    self.info.nic_addr,
                    e
                );
                Err(RadarError::Io(e))
            }
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> Result<(), RadarError> {
        if self.sock.is_none() {
            if self.unicast_mode {
                // Unicast topology, shared socket not yet available: drop the
                // command rather than open a colliding own socket. The report
                // loop retries socket creation and will supply the shared one.
                log::warn!(
                    "{}: Dropping command in unicast mode when shared socket is not available",
                    self.key
                );
                return Ok(());
            }
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

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use crate::network::create_connected_send;

    // The command socket (start_socket) relays through an Axiom to a WiFi
    // radar, so it must carry TTL > 1 or the relay drops it (issue #160).
    // start_socket needs a full RadarInfo to build, so exercise the socket
    // configuration it performs directly: a create_connected_send socket must
    // accept both the multicast and unicast relay TTL and report them back.
    #[tokio::test]
    async fn command_socket_accepts_relay_ttl() {
        let dst = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 1), 5800);
        let sock = create_connected_send(&dst, &Ipv4Addr::UNSPECIFIED)
            .expect("command send socket should be creatable");

        sock.set_multicast_ttl_v4(super::super::RAYMARINE_RELAY_TTL)
            .expect("multicast TTL settable");
        sock.set_ttl(super::super::RAYMARINE_RELAY_TTL)
            .expect("unicast TTL settable");

        assert_eq!(
            sock.multicast_ttl_v4().unwrap(),
            super::super::RAYMARINE_RELAY_TTL
        );
        assert_eq!(sock.ttl().unwrap(), super::super::RAYMARINE_RELAY_TTL);
    }
}
