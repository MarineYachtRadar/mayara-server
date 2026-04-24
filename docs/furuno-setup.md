# Furuno Radar Setup

This guide covers network configuration for all Furuno radar families supported by Mayara: DRS-NXT, DRS4W, DRS/X-Class, and FAR commercial series.

## Network Requirements

All Furuno radars communicate on the `172.31.0.0/16` subnet. The machine running Mayara **must** have an IP address on this subnet or the radar will not be detected.

Recommended configuration:
- IP address: `172.31.3.150` (or any unused address in `172.31.x.x`)
- Subnet mask: `255.255.0.0`
- No default gateway required (local subnet only)

Mayara's _Network_ page will show a warning if no interface has an address in the required range.

### DRS4W WiFi

The DRS4W ("1st Watch") creates its own WiFi network. Connect the Mayara machine to the radar's WiFi access point and start Mayara with `--allow-wifi`.

Multiple concurrent clients are allowed; you can use the standard Marine Radar iOS application alongside Mayara.

## DRS / DRS-NXT Series

DRS radars (DRS4D-NXT, DRS6A-NXT, DRS12A-NXT, DRS25A-NXT, and older DRS/X-Class models) work out of the box once the IP subnet is configured. No mode changes are needed on the radar itself.

The radar broadcasts discovery beacons and Mayara detects the model automatically from the beacon data.

### DRS Model Detection

Mayara identifies DRS models from the 7-digit part code in the `$N96` Modules response:

| Part Code | Model       |
| --------- | ----------- |
| 0359235   | DRS         |
| 0359338   | DRS4DL      |
| 0359367   | DRS4DL      |
| 0359360   | DRS4DNXT    |
| 0359329   | DRS4W       |
| 0359421   | DRS6ANXT    |
| 0359355   | DRS6AXCLASS |

## FAR Series (FAR-2xx7, FAR-15x3, FAR-3000)

### IMO Mode Configuration

The FAR-2xx7 must be set to **IMO Mode B, C, or W** for network connectivity. Mode W is recommended. If the radar is in Mode A (standalone), it will not respond to network commands.

To change the IMO mode on the FAR-2xx7:
1. Hold the **HL OFF** button
2. While holding HL OFF, press **MENU** 5 times
3. Navigate to **Installation** → **Type**
4. Select **W** (recommended), **B**, or **C**
5. Restart the radar

### FAR Model Detection

Mayara identifies FAR models from the 7-digit part code in the `$N96` Modules response:

| Part Code | Model    |
| --------- | -------- |
| 0359397   | FAR-14x6 |
| 0359255   | FAR-14x7 |
| 0359321   | FAR-14x7 |
| 0359344   | FAR-15x3 |
| 0359204   | FAR-21x7 |
| 0359560   | FAR-21x7 |
| 0359281   | FAR-3000 |
| 0359286   | FAR-3000 |
| 0359477   | FAR-3000 |

Unrecognized part codes still work with default capabilities. Please report the part code and model so it can be added.

## Troubleshooting

**Radar not detected:**
1. Verify the Mayara machine has a `172.31.x.x` IP address
2. Check that the Ethernet cable is connected to the radar's network port
3. For FAR-2xx7: verify IMO mode is set to W (not A)
4. For DRS4W: make sure you are connected to the radar's WiFi and started Mayara with `--allow-wifi`
5. Check firewall rules — the radar uses UDP broadcast and multicast

**FAR shows "Unknown" model:**
The part code is not in the lookup table. The radar will work with default capabilities. Report the part code from the log output so it can be added.
