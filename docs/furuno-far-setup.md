# Furuno FAR Radar Setup

This guide covers connecting mayara to Furuno FAR-series commercial radars
(FAR-2xx7, FAR-15x3, FAR-3000).

## Network Requirements

Furuno radars use the `172.31.0.0/16` subnet for all communication. The
machine running mayara **must** have an IP address on this subnet.

Recommended configuration:
- IP address: `172.31.3.150` (or any unused address in `172.31.x.x`)
- Subnet mask: `255.255.0.0`
- No default gateway required (local subnet only)

The radar broadcasts discovery beacons on `172.31.255.255:10010` and streams
echo data via multicast on `239.255.0.2:10024`. Login and control commands
use TCP on port 10010. If the mayara machine is not on the `172.31.0.0/16`
subnet, it will not receive beacon broadcasts and the radar will not be
detected.

## FAR-2xx7 IMO Mode Configuration

The FAR-2xx7 must be set to **IMO Mode B, C, or W** for network
connectivity with external software. Mode W is recommended.

If the radar is in Mode A (standalone), it will not respond to network
commands and will not be detected by mayara.

To change the IMO mode on the FAR-2xx7:
1. Hold the **HL OFF** button
2. While holding HL OFF, press **MENU** 5 times
3. Navigate to **Installation** → **Type**
4. Select **W** (recommended), **B**, or **C**
5. Restart the radar for the change to take effect

## Model Detection

Mayara identifies the radar model from the 7-digit part code in the `$N96`
Modules response. Known FAR part codes:

| Part Code | Model |
|-----------|-------|
| 0359204 | FAR-21x7 |
| 0359560 | FAR-21x7 |
| 0359321 | FAR-14x7 |
| 0359397 | FAR-14x6 |
| 0359344 | FAR-15x3 |
| 0359281 | FAR-3000 |
| 0359286 | FAR-3000 |
| 0359477 | FAR-3000 |

If your radar reports an unrecognized part code, mayara will still detect
and operate the radar with default capabilities. Please report the part
code and model name so it can be added to the lookup table.

## Troubleshooting

**Radar not detected:**
1. Verify the mayara machine has a `172.31.x.x` IP address
2. Check that the Ethernet cable is connected to the radar's network port
3. For FAR-2xx7: verify IMO mode is set to W (not A)
4. Check firewall rules — UDP 10010/10024 and TCP 10010 must be open
5. Run `tcpdump -i <interface> udp port 10010` to verify beacon packets
   are arriving

**Radar detected but shows "Unknown" model:**
The part code is not in the lookup table. The radar will work with default
capabilities. Report the part code (from the log output) so it can be
added.

**No echo data:**
Verify UDP port 10024 is not blocked. FAR radars use multicast
`239.255.0.2:10024` — the network interface must support multicast and
the OS must allow multicast group joins.
