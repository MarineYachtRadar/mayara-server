# Navico Radar Protocol Documentation

This document describes the Navico radar network protocol as reverse-engineered from
network captures and the mayara-lib implementation.

## Supported Models

- **BR24**: Original Broadband Radar (2009+)
- **3G**: Third generation radome radar
- **4G**: Fourth generation with dual range capability
- **HALO**: High-definition series with Doppler support (HALO 20, 20+, 24, 3, 4, 6)

## Network Architecture

Navico radars use UDP multicast for discovery and data transmission.

### Multicast Addresses

| Address | Port | Purpose |
|---------|------|---------|
| 236.6.7.4 | 6768 | BR24 beacon discovery |
| 236.6.7.5 | 6878 | Gen3/Gen4/HALO beacon discovery |
| 239.238.55.73 | 7527 | Navigation info (heading, position) |
| 236.6.7.20 | 6690 | Speed data A |
| 236.6.7.15 | 6005 | Speed data B |

### Dynamic Addresses from Beacon

The beacon response contains radar-specific multicast addresses for:
- Spoke data (radar image)
- Report data (status, controls)
- Command sending

## Device Discovery

### Address Request Packet (2 bytes)

Send to beacon multicast address to trigger radar responses:
```
01 B1
```

### Beacon Response Header

All beacon responses start with:
```
01 B2
```

### Beacon Packet Structures

#### BR24 Beacon (unique format)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 2 | ID (0x01 0xB2) |
| 2 | 16 | Serial number (ASCII, null-terminated) |
| 18 | 6 | Radar IP:port |
| ... | ... | Additional addresses |
| +10 | 6 | Report multicast address |
| +4 | 6 | Command send address |
| +4 | 6 | Data multicast address |

Note: BR24 has different field order than newer models.

#### Single-Range Beacon (3G, Halo 20)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 2 | ID (0x01 0xB2) |
| 2 | 16 | Serial number (ASCII, null-terminated) |
| 18 | 6 | Radar IP:port |
| ... | ... | Filler and additional addresses |
| +10 | 6 | Data multicast address |
| +4 | 6 | Command send address |
| +4 | 6 | Report multicast address |

#### Dual-Range Beacon (4G, HALO 20+, 24, 3, 4, 6)

Same as single-range, but with two radar endpoint sections (A and B) for
independent control of short-range and long-range modes.

### Network Address Format

Addresses are stored as 6 bytes:
```
struct NetworkSocketAddrV4 {
    addr: [u8; 4],  // IP address bytes
    port: [u8; 2],  // Port (big-endian)
}
```

## Radar Characteristics

| Model | Spokes | Spoke Length | Pixels | Doppler |
|-------|--------|--------------|--------|---------|
| BR24 | 2048 | 1024 | 16 (4-bit) | No |
| 3G | 2048 | 1024 | 16 (4-bit) | No |
| 4G | 2048 | 1024 | 16 (4-bit) | No |
| HALO | 2048 | 1024 | 16 (4-bit) | Yes |

### Pixel Data Format

- 4 bits per pixel (values 0-15)
- Packed 2 pixels per byte (low nibble first, then high nibble)
- 512 bytes per spoke → 1024 pixels when unpacked

### HALO Doppler Mode

HALO radars can encode Doppler information in pixel values:
- `0x0F` = Approaching target
- `0x0E` = Receding target
- Other values = Normal radar return intensity

Doppler modes:
| Value | Mode |
|-------|------|
| 0 | None (Doppler disabled) |
| 1 | Both (show approaching and receding) |
| 2 | Approaching only |

## Spoke Data Protocol

### Frame Structure

Each UDP packet contains up to 32 spokes:

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 8 | Frame header |
| 8 | 536 | Spoke 1 (24-byte header + 512-byte data) |
| 544 | 536 | Spoke 2 |
| ... | ... | Up to 32 spokes |

### BR24/3G Spoke Header (24 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Header length (24) |
| 1 | 1 | Status (0x02 or 0x12) |
| 2 | 2 | Scan number |
| 4 | 4 | Mark (BR24: 0x00, 0x44, 0x0d, 0x0e) |
| 8 | 2 | Angle (0-4095, divide by 2 for 0-2047) |
| 10 | 2 | Heading (with RI-10/11 interface) |
| 12 | 4 | Range |
| 16 | 8 | Unknown |

### 4G/HALO Spoke Header (24 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Header length (24) |
| 1 | 1 | Status (0x02 or 0x12) |
| 2 | 2 | Scan number |
| 4 | 2 | Mark |
| 6 | 2 | Large range |
| 8 | 2 | Angle (0-4095, divide by 2 for 0-2047) |
| 10 | 2 | Heading (0x4000 flag = true heading) |
| 12 | 2 | Small range (or 0xFFFF) |
| 14 | 2 | Rotation speed (or 0xFFFF) |
| 16 | 8 | Unknown |

### Range Calculation

**BR24/3G:**
```
range_meters = (raw_range & 0xFFFFFF) * (10.0 / 1.414)
```

**4G/HALO:**
```
if large_range == 0x80:
    if small_range == 0xFFFF:
        range = 0
    else:
        range = small_range / 4
else:
    range = (large_range * small_range) / 512
```

### Heading Extraction

Heading value contains flags:
- Bit 14 (0x4000): True heading flag
- Bits 0-11: Heading value (0-4095 for 360 degrees)

```rust
fn is_heading_true(x: u16) -> bool { (x & 0x4000) != 0 }
fn extract_heading(x: u16) -> u16 { x & 0x0FFF }
```

## Report Protocol (UDP)

Reports are received on the report multicast address.

### Report Identification

All reports have a 2-byte header:
- Byte 0: Report type
- Byte 1: Command (0xC4 for reports, 0xC6 for other)

### Report Types

| Type | Size | Description |
|------|------|-------------|
| 0x01 | 18 | Radar status (transmit/standby) |
| 0x02 | 99 | Control values (gain, sea, rain, etc.) |
| 0x03 | 129 | Model info (model, hours, firmware) |
| 0x04 | 66 | Installation settings (bearing, antenna height) |
| 0x06 | 68/74 | Blanking zones and radar name |
| 0x08 | 18/21/22 | Advanced settings (scan speed, doppler) |

### Report 01 - Status (18 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x01) |
| 1 | 1 | Command (0xC4) |
| 2 | 1 | Status |
| 3 | 15 | Unknown |

Status values:
| Value | Status |
|-------|--------|
| 0 | Off |
| 1 | Standby |
| 2 | Transmit |
| 5 | Preparing/Warming |

### Report 02 - Controls (99 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x02) |
| 1 | 1 | Command (0xC4) |
| 2 | 4 | Range (decimeters) |
| 6 | 1 | Unknown |
| 7 | 1 | Mode |
| 8 | 1 | Gain auto (0=manual, 1=auto) |
| 9 | 3 | Unknown |
| 12 | 1 | Gain value (0-255) |
| 13 | 1 | Sea auto (0=off, 1=harbor, 2=offshore) |
| 14 | 3 | Unknown |
| 17 | 4 | Sea value |
| 21 | 1 | Unknown |
| 22 | 1 | Rain value |
| 23 | 11 | Unknown |
| 34 | 1 | Interference rejection |
| 35 | 3 | Unknown |
| 38 | 1 | Target expansion |
| 39 | 3 | Unknown |
| 42 | 1 | Target boost |
| 43 | 56 | Unknown |

### Report 03 - Model Info (129 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x03) |
| 1 | 1 | Command (0xC4) |
| 2 | 1 | Model byte |
| 3 | 31 | Unknown |
| 34 | 4 | Operating hours |
| 38 | 20 | Unknown |
| 58 | 32 | Firmware date (UTF-16LE) |
| 90 | 32 | Firmware time (UTF-16LE) |
| 122 | 7 | Unknown |

Model bytes:
| Value | Model |
|-------|-------|
| 0x00 | HALO |
| 0x01 | 4G |
| 0x08 | 3G |
| 0x0E, 0x0F | BR24 |

### Report 04 - Installation (66 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x04) |
| 1 | 1 | Command (0xC4) |
| 2 | 4 | Unknown |
| 6 | 2 | Bearing alignment (deci-degrees, signed) |
| 8 | 2 | Unknown |
| 10 | 2 | Antenna height (decimeters) |
| 12 | 7 | Unknown |
| 19 | 1 | Accent light (HALO only) |
| 20 | 46 | Unknown |

### Report 06 - Blanking Zones (68 or 74 bytes)

Contains radar name and up to 4 no-transmit zone definitions.

Each zone (5 bytes):
| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Enabled |
| 1 | 2 | Start angle (deci-degrees) |
| 3 | 2 | End angle (deci-degrees) |

### Report 08 - Advanced Settings (18/21/22 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x08) |
| 1 | 1 | Command (0xC4) |
| 2 | 1 | Sea state |
| 3 | 1 | Local interference rejection |
| 4 | 1 | Scan speed |
| 5 | 1 | Sidelobe suppression auto |
| 6 | 3 | Unknown |
| 9 | 1 | Sidelobe suppression value |
| 10 | 2 | Unknown |
| 12 | 1 | Noise rejection |
| 13 | 1 | Target separation |
| 14 | 1 | Sea clutter (HALO) |
| 15 | 1 | Auto sea clutter (HALO, signed) |
| 16 | 2 | Unknown |

Extended fields (21+ bytes, HALO only):
| Offset | Size | Description |
|--------|------|-------------|
| 18 | 1 | Doppler state |
| 19 | 2 | Doppler speed threshold (cm/s, 0-1594) |

## Command Protocol (UDP)

Commands are sent to the command address received in the beacon.

### Command Format

Commands are variable-length byte sequences sent via UDP.

### Request Reports

| Command | Response |
|---------|----------|
| `04 C2` | Report 03 (model info) |
| `01 C2` | Reports 02, 03, 04, 07, 08 |
| `02 C2` | Report 04 |
| `03 C2` | Reports 02 and 08 |

### Stay Alive

```
A0 C1
```
Keeps radar A active in dual-range mode.

### Transmit/Standby (0x00, 0x01 C1)

```
00 C1 01        # Prepare for status change
01 C1 XX        # XX: 0=standby, 1=transmit
```

### Range (0x03 C1)

```
03 C1 DD DD DD DD
```
DD DD DD DD = Range in decimeters (little-endian i32)

### Bearing Alignment (0x05 C1)

```
05 C1 VV VV
```
VV VV = Alignment in deci-degrees (little-endian i16, 0-3599)

### Gain (0x06 C1)

```
06 C1 00 00 00 00 AA AA AA AA VV
```
- AA AA AA AA = Auto mode (0=manual, 1=auto, little-endian u32)
- VV = Value (0-255, maps to 0-100%)

### Sea Clutter (0x06 C1, subtype 0x02 - non-HALO)

```
06 C1 02 AA AA AA AA VV VV VV VV
```
- AA = Auto mode (big-endian)
- VV = Value (big-endian u32)

### Sea Clutter (0x11 C1 - HALO)

Mode selection:
```
11 C1 XX 00 00 0Y
```
- XX: 0=manual mode, 1=auto mode
- Y: 1=mode command

Manual value:
```
11 C1 00 VV VV 02
```
- VV = Value (0-100)

Auto adjust:
```
11 C1 01 00 AA 04
```
- AA = Auto adjustment (signed i8, -50 to +50)

### Rain Clutter (0x06 C1, subtype 0x04)

```
06 C1 04 00 00 00 00 00 00 00 VV
```
VV = Value (0-255)

### Sidelobe Suppression (0x06 C1, subtype 0x05)

```
06 C1 05 00 00 00 AA 00 00 00 VV
```
- AA = Auto (0=manual, 1=auto)
- VV = Value (0-255)

### Interference Rejection (0x08 C1)

```
08 C1 VV
```
VV = Level (0=off, 1=low, 2=medium, 3=high)

### Target Expansion (0x09 C1 or 0x12 C1)

```
09 C1 VV        # Non-HALO
12 C1 VV        # HALO
```
VV = Level (0=off, 1=on, 2=high for HALO)

### Target Boost (0x0A C1)

```
0A C1 VV
```
VV = Level (0=off, 1=low, 2=high)

### Sea State (0x0B C1)

```
0B C1 VV
```
VV = State (0=calm, 1=moderate, 2=rough)

### No Transmit Zones (0x0D C1, 0xC0 C1)

Enable/disable zone:
```
0D C1 SS 00 00 00 EE
```
- SS = Sector (0-3)
- EE = Enabled (0=off, 1=on)

Set zone angles:
```
C0 C1 SS 00 00 00 EE ST ST EN EN
```
- SS = Sector (0-3)
- EE = Enabled
- ST ST = Start angle (deci-degrees, little-endian i16)
- EN EN = End angle (deci-degrees, little-endian i16)

### Local Interference Rejection (0x0E C1)

```
0E C1 VV
```
VV = Level (0=off, 1=low, 2=medium, 3=high)

### Scan Speed (0x0F C1)

```
0F C1 VV
```
VV = Speed (0=normal, 1=fast)

### Mode (0x10 C1)

```
10 C1 VV
```
VV = Mode (0=custom, 1=harbor, 2=offshore, 3=weather, etc.)

### Noise Rejection (0x21 C1)

```
21 C1 VV
```
VV = Level (0=off, 1=low, 2=medium, 3=high)

### Target Separation (0x22 C1)

```
22 C1 VV
```
VV = Level (0=off, 1=low, 2=medium, 3=high)

### Doppler (0x23 C1 - HALO only)

```
23 C1 VV
```
VV = Mode (0=off, 1=both, 2=approaching)

### Doppler Speed Threshold (0x24 C1 - HALO only)

```
24 C1 TT TT
```
TT TT = Speed threshold * 16 (little-endian u16, in knots)

### Antenna Height (0x30 C1)

```
30 C1 01 00 00 00 HH HH 00 00
```
HH HH = Height in decimeters (little-endian u16)

### Accent Light (0x31 C1 - HALO only)

```
31 C1 VV
```
VV = Level (0=off, 1-3=brightness levels)

## Navigation Info Protocol

### HALO Heading Packet (72 bytes)

Sent on multicast 239.238.55.73:7527

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Marker ('NKOE') |
| 4 | 4 | Preamble (00 01 90 02) |
| 8 | 2 | Counter (big-endian) |
| 10 | 26 | Unknown |
| 36 | 4 | Subtype (12 F1 01 00 for heading) |
| 40 | 8 | Timestamp (millis since 1970) |
| 48 | 18 | Unknown |
| 66 | 2 | Heading (0.1 degrees) |
| 68 | 4 | Unknown |

### HALO Navigation Packet (72 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Marker ('NKOE') |
| 4 | 4 | Preamble (00 01 90 02) |
| 8 | 2 | Counter (big-endian) |
| 10 | 26 | Unknown |
| 36 | 4 | Subtype (02 F8 01 00 for navigation) |
| 40 | 8 | Timestamp (millis since 1970) |
| 48 | 18 | Unknown |
| 66 | 2 | COG (0.01 radians, 0-63488) |
| 68 | 2 | SOG (0.01 m/s) |
| 70 | 2 | Unknown |

### HALO Speed Packet (23 bytes)

Sent on multicast 236.6.7.20:6690

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 6 | Marker (01 D3 01 00 00 00) |
| 6 | 2 | SOG (m/s) |
| 8 | 6 | Unknown |
| 14 | 2 | COG |
| 16 | 7 | Unknown |

## Keep-Alive / Report Requests

Reports should be requested every 5 seconds to keep the radar active and
receive current control values:

```rust
send(&[0x04, 0xC2]);  // Request Report 03
send(&[0x01, 0xC2]);  // Request multiple reports
send(&[0xA0, 0xC1]);  // Stay on A (dual-range)
```

### Complete Stay-Alive Sequence

For robust operation, send the full stay-alive sequence periodically:

| Command | Bytes | Purpose |
|---------|-------|---------|
| Stay Alive A | `A0 C1` | Keep radar A active (dual-range) |
| Request Reports | `03 C2` | Request reports 02 and 08 |
| Request Model | `04 C2` | Request report 03 (model info) |
| Request All | `05 C2` | Request additional reports |
| Request Install | `0A C2` | Request installation settings |

**Timing:**
- HALO radars: Send every 50-100ms for responsive operation
- BR24/3G/4G: Every 1-5 seconds is sufficient

### Dual-Range Stay-Alive

For dual-range radars (4G, HALO), both radar channels need keep-alive:

```rust
// Radar A (short range)
send_to_addr_a(&[0xA0, 0xC1]);

// Radar B (long range) - if using dual range
send_to_addr_b(&[0xA0, 0xC1]);
```

### TX On/Off Sequence

Transmit commands require a two-part sequence:

```
// Transmit OFF (Standby)
00 C1 01        // Prepare
01 C1 00        // Execute standby

// Transmit ON
00 C1 01        // Prepare
01 C1 01        // Execute transmit
```

Both parts must be sent; the prepare command (0x00 C1 01) primes the radar
for a state change.

## Detailed Beacon Structure (from signalk-radar)

The full 01B2 beacon packet (222+ bytes) contains multiple address pairs:

```go
struct RadarReport_01B2 {
    Id:          u16,           // 0x01B2
    Serialno:    [16]u8,        // Serial number
    Addr0:       Address,       // 6 bytes
    U1:          [12]u8,        // Filler
    Addr1:       Address,
    U2:          [4]u8,
    Addr2:       Address,
    U3:          [10]u8,
    Addr3:       Address,
    U4:          [4]u8,
    Addr4:       Address,
    U5:          [10]u8,
    AddrDataA:   Address,       // Spoke data for radar A
    U6:          [4]u8,
    AddrSendA:   Address,       // Command address for radar A
    U7:          [4]u8,
    AddrReportA: Address,       // Report address for radar A
    U8:          [10]u8,
    AddrDataB:   Address,       // Spoke data for radar B (dual-range)
    U9:          [4]u8,
    AddrSendB:   Address,       // Command address for radar B
    U10:         [4]u8,
    AddrReportB: Address,       // Report address for radar B
    U11:         [10]u8,
    Addr11-16:   Address × 6,   // Additional addresses (unknown purpose)
}
```

## Range Calculation Details

### 3G/4G Models

```
if Largerange == 0x80:
    if Smallrange == 0xFFFF:
        range_meters = 0
    else:
        range_meters = Smallrange / 4
else:
    range_meters = Largerange * 64
```

### HALO Models

```
if Largerange == 0x80:
    if Smallrange == 0xFFFF:
        range_meters = 0
    else:
        range_meters = Smallrange / 4
else:
    range_meters = Largerange * (Smallrange / 512)
```

The HALO calculation provides variable resolution based on the smallrange value.

## Doppler Pixel Mapping

For displays supporting Doppler visualization, pixel values are remapped:

| Raw Value | Doppler Mode: None | Doppler Mode: Both | Doppler Mode: Approaching |
|-----------|-------------------|-------------------|--------------------------|
| 0x00-0x0D | Signal intensity | Signal intensity | Signal intensity |
| 0x0E | Signal intensity | Receding target | Signal intensity |
| 0x0F | Signal intensity | Approaching target | Approaching target |

Color scheme (16-level radar + extras):
- Pixel 0: Transparent (no signal)
- Pixels 1-14: Blue → Green → Red gradient (signal strength)
- Pixel 15: Border/outline (gray)
- Pixel 16: Doppler Approaching (cyan #00C8C8)
- Pixel 17: Doppler Receding (light blue #90D0F0)
- Pixels 18-49: History/trail fade (grayscale)

## References

- mayara-lib source: `src/brand/navico/`
- mayara-core protocol: `src/protocol/navico.rs`
- signalk-radar Go implementation: `radar-server/radar/navico/`
- OpenCPN radar_pi plugin (original reverse engineering)
- Network captures from various Navico radar installations
- Sample PCAP: `signalk-radar/demo/samples/halo_and_0183.pcap`
