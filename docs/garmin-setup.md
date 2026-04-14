# Garmin Radar Setup

This guide covers connecting Mayara to Garmin marine radars: GMR HD, xHD, xHD2, xHD3, Fantom, and Fantom Pro.

## Network Requirements

Garmin radars communicate on the `172.16.0.0/12` subnet. The machine running Mayara **must** have an IP address in the `172.16.x.x` – `172.31.x.x` range or the radar will not be detected.

Recommended configuration:
- IP address: `172.16.3.150` (or any unused address in `172.16.x.x`)
- Subnet mask: `255.240.0.0`
- No default gateway required

Mayara's _Network_ page will show a warning if no interface has an address in the required range.

## Supported Models

| Family     | Models                   | Dual Range | Doppler |
| ---------- | ------------------------ | ---------- | ------- |
| HD         | GMR 18 HD, GMR 24 HD     | No         | No      |
| xHD        | GMR 18 xHD, GMR 24 xHD   | No         | No      |
| xHD2       | xHD2 dome and open array | Yes        | No      |
| xHD3       | xHD3 open array          | Yes        | No      |
| Fantom     | Fantom 18, 24, 124, 126  | Yes        | Yes     |
| Fantom Pro | Fantom 54, 56            | Yes        | Yes     |

Model detection is automatic via the radar's capability bitmap and CDM heartbeat.

## Dual-Range

xHD2, xHD3, Fantom, and Fantom Pro support dual-range operation. Mayara shows each range as a separate radar (Range A and Range B) in the radar list.

## MotionScope (Doppler)

Fantom and Fantom Pro radars support MotionScope, which color-codes approaching and receding targets. The radar performs the Doppler processing internally and does not require heading data for MotionScope to function. Mayara displays the Doppler color-coding automatically when the radar provides it.

A heading sensor is recommended for accurate radar overlay on charts and for MARPA target tracking. In a Garmin system, the radar connects via Ethernet to the chartplotter, which bridges heading data from the NMEA 2000 network. When using Mayara without a Garmin chartplotter, there is no path for heading data to reach the radar — but MotionScope should
still work without it. Please report to us whether it does!

## Troubleshooting

**Radar not detected:**
- Verify the Mayara machine has an IP address in the `172.16.0.0/12` range (e.g., `172.16.3.150`).
- Check the _Network_ page in the Mayara GUI to see if the subnet requirement is satisfied.
- Ensure the Ethernet cable is connected and the radar is powered on.
- Try specifying the network interface: `mayara-server -i eth0`

**Wrong model detected:**
- Garmin model detection relies on the capability bitmap broadcast by the radar. If the detected model name seems wrong, please report it along with the log output from `mayara-server -vv`.
