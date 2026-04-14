# Raymarine Radar Setup

This guide covers connecting Mayara to Raymarine radars: Quantum, RD-series, Magnum, and Cyclone.

## Network Requirements

Raymarine radars have no specific IP subnet requirement, but they do require a **DHCP server** on the network. The radar obtains its IP address via DHCP. The DHCP server can be:

- A Raymarine chartplotter (MFD) on the same network
- A network router with DHCP enabled
- A DHCP service running on the Mayara machine or another computer

Without DHCP, the radar will not acquire an IP address and will not be detected.

## Supported Models

### Quantum (solid-state FMCW)

| Model          | Doppler |
| -------------- | ------- |
| Quantum Q24C   | No      |
| Quantum Q24D   | Yes     |
| Quantum 2 Q24D | Yes     |
| Cyclone        | Yes     |
| Cyclone Pro    | Yes     |

Quantum radars are auto-detected on both wired Ethernet and WiFi.

**WiFi support:** Quantum radars can connect via WiFi. Start Mayara with `--allow-wifi` to enable radar discovery on wireless interfaces. Note that WiFi performance may be limited for sustained radar data, but Quantum radars send quite small radar images so it should be
okay. Let us know how it works for you!

### RD Series (magnetron)

| Model              | Notes                                   |
| ------------------ | --------------------------------------- |
| RD418HD, RD424HD   | HD resolution (1024 samples/spoke)      |
| RD418D, RD424D     | Standard resolution (512 samples/spoke) |
| Magnum 4kW, 12kW   | Open array                              |
| Open Array HD, SHD | High-definition open array              |

RD-series radars are wired Ethernet only.

## Troubleshooting

**Radar not detected:**
- Verify a DHCP server is active on the radar's network.
- Check that the Mayara machine is on the same network as the radar.
- For Quantum via WiFi: ensure `--allow-wifi` is specified.
- Try specifying the network interface: `mayara-server -i eth0`
- Use `-vv` to see discovery traffic in the log.

**Quantum WiFi unreliable:**
- WiFi performance depends on signal strength and interference. For best results, use a wired connection when possible.
- If spokes are missing, move the Mayara machine closer to the radar or switch to a wired connection.
