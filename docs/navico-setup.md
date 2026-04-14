# Navico Radar Setup

This guide covers connecting Mayara to Navico radars (Simrad, Lowrance, B&G): BR24, 3G, 4G, and all HALO models.

## Network Requirements

Navico radars have no specific IP subnet requirement. The radar and the Mayara machine just need to be on the same Ethernet network. The radar uses multicast for discovery and data, so the network must support multicast (any standard Ethernet switch does).

### Why WiFi doesn't work for radar data

Radar data is sent as multicast UDP. On wired Ethernet, multicast is handled at wire speed with no overhead. On WiFi, multicast frames cannot use the high data rates negotiated between the access point and each client, because multicast has no single recipient to negotiate with. Instead, the access point must transmit multicast frames at the **base rate** — the lowest mandatory rate of the WiFi standard — to guarantee that every associated client can receive them.

The base rates for common WiFi standards:

| Standard | Band | Base Rate | Typical Max Rate |
|----------|------|-----------|-----------------|
| 802.11b | 2.4 GHz | 1 Mbit/s | 11 Mbit/s |
| 802.11g | 2.4 GHz | 6 Mbit/s | 54 Mbit/s |
| 802.11n (Wi-Fi 4) | 2.4 / 5 GHz | 6 Mbit/s | 600 Mbit/s |
| 802.11ac (Wi-Fi 5) | 5 GHz | 6 Mbit/s | 6.9 Gbit/s |
| 802.11ax (Wi-Fi 6) | 2.4 / 5 / 6 GHz | 6 Mbit/s | 9.6 Gbit/s |

A Navico radar produces roughly 1 MB/s (~8 Mbit/s) of spoke data. At the 802.11b base rate of 1 Mbit/s this is impossible. Even at the 6 Mbit/s base rate of modern standards, the radar data alone consumes most of the available multicast bandwidth before accounting for WiFi framing overhead, leaving no room for retransmissions — and multicast has no retransmissions. The result is massive spoke loss.

This is why Mayara must be connected to the radar via wired Ethernet. You can then access the Mayara web interface from phones, tablets, or laptops over WiFi — Mayara serves the processed data as unicast HTTP/WebSocket, which uses the full negotiated WiFi rate.

Some access points allow raising the minimum multicast rate above the standard base rate. With a high enough multicast rate, radar data may technically fit — but even then, the multicast traffic consumes a large share of the WiFi airtime, slowing down all other WiFi users on the same access point. A wired connection for the radar path avoids this entirely.

## Supported Models

| Family          | Models                            | Dual Range           | Doppler |
| --------------- | --------------------------------- | -------------------- | ------- |
| BR24            | BR24                              | No                   | No      |
| 3G              | Broadband 3G                      | No                   | No      |
| 4G              | Broadband 4G                      | Yes                  | No      |
| HALO Dome       | HALO 20, HALO 20+, HALO 24        | Yes (except HALO 20) | Yes     |
| HALO Open Array | HALO 2000, 3000, 4000, 5000, 6000 | Yes                  | Yes     |

All models are auto-detected from the radar's beacon. No manual model selection is needed.

## HALO Heading and Navigation Data

HALO radars require heading data to enable Doppler/VelocityTrack. Mayara sends heading, position, and speed to the radar automatically when a navigation source is configured:

```sh
# Auto-discover Signal K server via mDNS
mayara-server

# Or specify a Signal K server address
mayara-server -n tcp:192.168.1.100:3000

# Or use NMEA 0183 input
mayara-server --nmea0183 -n udp:0.0.0.0:10110
```

Without heading data, HALO Doppler mode will not activate.

## Dual-Range (Multi-Scanner)

4G and most HALO radars support dual-range operation, where the radar runs two independent range windows simultaneously. Mayara shows each range as a separate radar (suffixed A and B). Both appear in the radar list and can be controlled independently.

## Troubleshooting

**Radar not detected:**
- Verify the Mayara machine is on the same wired Ethernet network as the radar.
- Check that the radar is powered on (the dome or array should be spinning or the status LED active).
- Try specifying the network interface: `mayara-server -i eth0`
- Use `-vv` to see discovery traffic in the log.

**Doppler not working (HALO):**
- Ensure a heading source is connected (Signal K server or NMEA 0183).
- Check that the radar firmware supports Doppler (most HALO firmware versions do).
- Verify heading data is arriving: look for heading values in the Mayara log output with `-v`.
