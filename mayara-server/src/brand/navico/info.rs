//! Navico navigation data sender
//!
//! Sends heading, SOG, and COG packets to Navico radars.
//! Uses formatting functions from mayara-core for packet construction.

use std::net::{Ipv4Addr, SocketAddrV4};
use tokio::net::UdpSocket;

use mayara_core::protocol::navico::{
    format_heading_packet, format_navigation_packet, format_speed_packet, INFO_ADDR, INFO_PORT,
    SPEED_ADDR_A, SPEED_ADDR_B, SPEED_PORT_A, SPEED_PORT_B,
};

use crate::navdata::{get_cog, get_heading_true, get_sog};
use crate::network::create_multicast_send;
use crate::radar::{RadarError, RadarInfo};

// Socket addresses constructed from core constants
fn info_socket_addr() -> SocketAddrV4 {
    SocketAddrV4::new(INFO_ADDR.parse().unwrap(), INFO_PORT)
}

fn speed_a_socket_addr() -> SocketAddrV4 {
    SocketAddrV4::new(SPEED_ADDR_A.parse().unwrap(), SPEED_PORT_A)
}

fn speed_b_socket_addr() -> SocketAddrV4 {
    SocketAddrV4::new(SPEED_ADDR_B.parse().unwrap(), SPEED_PORT_B)
}

// Socket index for the socket array
enum SocketIndex {
    HeadingAndNavigation = 0,
    SpeedA = 1,
    SpeedB = 2,
}

fn socket_address(index: usize) -> SocketAddrV4 {
    match index {
        0 => info_socket_addr(),
        1 => speed_a_socket_addr(),
        2 => speed_b_socket_addr(),
        _ => panic!("Invalid socket index"),
    }
}

pub(crate) struct Information {
    key: String,
    nic_addr: Ipv4Addr,
    sock: [Option<UdpSocket>; 3], // Heading/Navigation, Speed A, Speed B
    counter: u16,
}

impl Information {
    pub fn new(key: String, info: &RadarInfo) -> Self {
        Information {
            key,
            nic_addr: info.nic_addr,
            sock: [None, None, None],
            counter: 0,
        }
    }

    async fn start_socket(&mut self, index: usize) -> Result<(), RadarError> {
        if self.sock[index].is_some() {
            return Ok(());
        }
        let addr = socket_address(index);
        match create_multicast_send(&addr, &self.nic_addr) {
            Ok(sock) => {
                log::debug!("{} {} via {}: sending info", self.key, addr, &self.nic_addr);
                self.sock[index] = Some(sock);
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} {} via {}: create multicast failed: {}",
                    self.key,
                    addr,
                    &self.nic_addr,
                    e
                );
                Err(RadarError::Io(e))
            }
        }
    }

    pub async fn send(&mut self, index: usize, message: &[u8]) -> Result<(), RadarError> {
        self.start_socket(index).await?;

        if let Some(sock) = &self.sock[index] {
            sock.send(message).await.map_err(RadarError::Io)?;
            log::trace!("{}: sent {:02X?}", self.key, message);
        }

        Ok(())
    }

    async fn send_heading_packet(&mut self) -> Result<(), RadarError> {
        if let Some(heading) = get_heading_true() {
            let timestamp_ms = chrono::Utc::now().timestamp_millis();
            let packet = format_heading_packet(heading, self.counter, timestamp_ms);
            self.counter = self.counter.wrapping_add(1);
            self.send(SocketIndex::HeadingAndNavigation as usize, &packet)
                .await?;
        }
        Ok(())
    }

    async fn send_navigation_packet(&mut self) -> Result<(), RadarError> {
        if let (Some(sog), Some(cog)) = (get_sog(), get_cog()) {
            let timestamp_ms = chrono::Utc::now().timestamp_millis();
            let packet = format_navigation_packet(sog, cog, self.counter, timestamp_ms);
            self.counter = self.counter.wrapping_add(1);
            self.send(SocketIndex::HeadingAndNavigation as usize, &packet)
                .await?;
        }
        Ok(())
    }

    async fn send_speed_packet(&mut self) -> Result<(), RadarError> {
        if let (Some(sog), Some(cog)) = (get_sog(), get_cog()) {
            let packet = format_speed_packet(sog, cog);
            self.send(SocketIndex::SpeedA as usize, &packet).await?;
            self.send(SocketIndex::SpeedB as usize, &packet).await?;
        }
        Ok(())
    }

    pub(super) async fn send_info_requests(&mut self) -> Result<(), RadarError> {
        self.send_heading_packet().await?;
        self.send_navigation_packet().await?;
        self.send_speed_packet().await?;
        Ok(())
    }
}
