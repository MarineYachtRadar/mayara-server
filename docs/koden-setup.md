# Koden Radar Setup

This guide covers connecting Mayara to Koden marine radars via the RADARpc Ethernet control boxes (MDS-5R, MDS-6R, MDS-11R).

## Network Requirements

Koden radars communicate via UDP broadcast on port 10001. The machine running Mayara must be on the same subnet as the radar.

The default Koden radar IP is typically in the `192.168.0.x` range. Configure the machine running Mayara with a static IP on the same subnet.

## Supported Models

| Model Code | Model     | Power | Antenna        | Control box |
| ---------- | --------- | ----- | -------------- | ----------- |
| 0          | MDS-50R   | 2 kW  | Dome           | MDS-5R      |
| 1          | MDS-51R   | 4 kW  | Dome           | MDS-5R      |
| 2          | MDS-52R   | 4 kW  | Open Array     | MDS-5R      |
| 3          | MDS-61R   | 6 kW  | Open Array     | MDS-6R      |
| 4          | MDS-62R   | 12 kW | Open Array     | MDS-6R      |
| 5          | MDS-63R   | 25 kW | Open Array     | MDS-6R      |
| 6          | MDS-1R/8R | 2 kW  | Dome           | -           |
| 10         | MDS-10R   | 4 kW  | Open Array     | -           |
| 14         | MDS-9R    | 4 kW  | Dome           | -           |
| 15         | MDS-5R    | —     | Interface only | MDS-5R      |

Model detection is automatic via the model code response from the radar. From what we know, all control
boxes use exactly the same protocol so it should work as long as you have an Ethernet port on the MDS 
control box. We do not know what model code is reported by combos using the newer MDS-11R control box,
please report this.

Unfortunately, Koden radars with a Koden display unit all seem not to use Ethernet to provide features
such as dual display units, and thus are not supportable by _Mayara_.

## Controls

Mayara supports the following Koden controls:

- **Power** — standby / transmit
- **Gain** — manual (0–100%) or auto
- **Sea clutter (STC)** — manual (0–100%)
- **Rain clutter (FTC)** — manual (0–100%)
- **Sea state** — manual / auto / harbor mode
- **Interference rejection** — off / low / medium / high
- **Target expansion** — on / off
- **Scan speed** — normal / fast
- **Tune** — coarse tuning (0–255) with manual / auto mode
- **Fine tune** — fine tuning adjustment (0–15)
- **Pulse width** — short / long
- **Display timing** — trigger delay compensation for cable length (0–124)
- **No-transmit sector** — blanking sector start/end angles
- **Park position** — antenna park angle

## Troubleshooting

**Radar not detected**: Verify the machine and radar are on the same subnet. Koden radars use UDP broadcast on port 10001 — ensure no firewall is blocking this port. Try running with `--brand koden` to limit detection to Koden radars only.

**No spokes displayed**: The radar must be in transmit mode. Use the power control to switch from standby to transmit. New radars may require a warmup period before transmitting.

## Caveat

The current Koden support has NOT been tested AT ALL with real radars and depends on knowledge learned
from other sources. If you have a Koden radar, please contact us.
