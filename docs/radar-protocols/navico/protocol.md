# Navico Radar Protocol Documentation

This document describes the Navico radar network protocol as reverse-engineered from
network captures and the mayara-lib implementation.

## Supported Models

- **BR24**: Original FMCW (Frequency Modulation Continous Wave) Broadband Radar (2009+)
- **3G**: Third generation radome radar
- **4G**: Fourth generation with dual range capability
- **HALO**: Pulse compression series, most with Doppler support (HALO 20+, 24, 3, 4, 6, 20003/4/6, 3003/4/6) and one without Doppler (HALO 20)

## Network Architecture

Navico radars use UDP multicast for discovery and data transmission. It is irrelevant whether a DHCP server
is present, it will use an auto configured IPv4 address in range 169.254/16 as well as the address provided by
the DHCP server. In practice, because of the fact that all data is sent on addresses in the multicast ranges,
the actual IP address is not so relevant.

The disadvantage of IPv4 Multicast is that it works poorly over WiFi, as any form of broadcast or multicast means that the packages need to be sent at the lowest rate supported by any of the nodes. Even on 5 GHz you will see
spoke and command data dropouts. 

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

Note that when the doppler mode is "Approaching only" the value 0x0E is used as a normal value, and if
the doppler mode is "None" then both 0x0E and 0x0F are used as a normal echo strength value.

Doppler modes:
| Value | Mode |
|-------|------|
| 0 | None (Doppler disabled) |
| 1 | Both (show approaching and receding) |
| 2 | Approaching only |

## Control Categories

Radar settings are categorized by their purpose and persistence:

### Installation Settings (Report 04 and Report 08)
These are configured once during radar installation and rarely changed:
- **Bearing alignment** - Corrects for antenna mounting offset (deci-degrees, 0-3599) [Report 04]
- **Antenna height** - Height above waterline in decimeters (affects sea clutter calculations) [Report 04]
- **Accent light** - HALO pedestal LED brightness (0-3, HALO only) [Report 04]
- **Local interference rejection** - Reduce local interference (off/low/medium/high) [Report 08]
- **Sidelobe suppression** - Reduce sidelobe artifacts (0-100%, auto or manual) [Report 08]

### Runtime Controls (Report 02)
Operational settings adjusted during normal use (per-radar on dual-range systems):
- **Mode** - Presets for common uses (Halo only: "Custom", "Harbor", "Offshore", "Buoy", "Weather", "Bird")
             Anything other than "Custom" will make certain advanced settings inaccessible as they are fully
             defined by the mode.
- **Gain** - Signal amplification (0-100% manual or auto)
- **Sea clutter** - Sea return suppression (manual 0-100%, or auto depending on model: 4G and below: harbor/offshore, HALO: Auto-50 to Auto+50 and see Sea State)
- **Rain clutter** - Precipitation suppression (0-100%, no auto mode)
- **Interference rejection** - Filter other radar interference (off/low/medium/high)
- **Target expansion** - Make small targets more visible (off/on, HALO: off/low/medium/high)
- **Target boost** - Amplify weak targets (off/low/high)
- **Guard zones** - Up to 2 zones per radar, sector or full-circle shape, sensitivity shared within same radar

### Advanced Settings (Report 08)
Performance tuning options (per-radar on dual-range systems):
- **Scan speed** - Antenna rotation speed (BR24, 3G: normal/fast, 4G: normal/medium/fast, HALO: normal, medium, medium-fast, fast)
- **Sea state** - Sea condition preset (calm/moderate/rough, HALO only)
- **Noise rejection** - Filter noise (off/low/high, HALO: off/low/medium/high)
- **Target separation** - Distinguish close targets (off/low/medium/high)
- **Doppler mode** - Motion detection (off/both/approaching, HALO only)

### Blanking Zones (Report 06) (HALO only)
No-transmit sectors to protect crew or equipment:
- Up to 4 sectors with start/end angles

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
| 0x07 | 188 | Statistics/diagnostics (4G verified) |
| 0x08 | 18/21/22 | Advanced settings (scan speed, doppler) |
| 0x09 | 13 | Unknown (tuning/calibration?) |

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
| 0 | Off (not observed - radar stops sending when powered off) |
| 1 | Standby |
| 2 | Transmit |
| 5 | Preparing/Spinning up (not observed on 4G model, possibly HALO only) |

**Power-off behavior:** When the radar is powered off, it simply stops sending
packets. There is no special "powering down" status - the radar goes silent
immediately. This is expected since the radar has no power to transmit anything.

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
| 13 | 1 | Sea auto (0=manual, 1=harbor, 2=offshore) |
| 14 | 3 | Unknown |
| 17 | 1 | Sea value (0-255) |
| 18 | 3 | Unknown |
| 21 | 1 | Unknown |
| 22 | 1 | Rain value (0-255, no auto mode) |
| 23 | 11 | Unknown |
| 34 | 1 | Interference rejection |
| 35 | 3 | Unknown |
| 38 | 1 | Target expansion |
| 39 | 3 | Unknown |
| 42 | 1 | Target boost |
| 43 | 11 | Unknown |
| 54 | 1 | Guard zone sensitivity (0-255, shared by both zones) |
| 55 | 1 | Guard zone 1 enabled (0=off, 1=on) |
| 56 | 1 | Guard zone 2 enabled (0=off, 1=on) |
| 57 | 4 | Unknown (zeros) |
| 61 | 1 | Guard zone 1 inner range (meters) |
| 62 | 3 | Unknown (zeros) |
| 65 | 1 | Guard zone 1 outer range (meters) |
| 66 | 3 | Unknown (zeros) |
| 69 | 2 | Guard zone 1 bearing (deci-degrees, u16 LE) |
| 71 | 2 | Guard zone 1 width (deci-degrees, u16 LE) |
| 73 | 4 | Unknown (zeros) |
| 77 | 1 | Guard zone 2 inner range (meters) |
| 78 | 3 | Unknown (zeros) |
| 81 | 1 | Guard zone 2 outer range (meters) |
| 82 | 3 | Unknown (zeros) |
| 85 | 2 | Guard zone 2 bearing (deci-degrees, u16 LE) |
| 87 | 2 | Guard zone 2 width (deci-degrees, u16 LE) |
| 89 | 10 | Unknown |

**Verified values (4G radar):**

Gain (offsets 8, 12):
- Auto: offset 8 = `01`, offset 12 = auto-calculated value
- Manual 0%: offset 8 = `00`, offset 12 = `00`
- Manual 100%: offset 8 = `00`, offset 12 = `FF`

Sea clutter (offsets 13, 17):
| Mode | Offset 13 |
|------|-----------|
| Manual | `00` |
| Harbor (auto) | `01` |
| Offshore (auto) | `02` |
- Manual value: offset 17 = 0-255 (percentage × 255 / 100)

Rain clutter (offset 22):
- No auto mode available
- Value: 0-255 (percentage × 255 / 100)
- 0% = `00`, 64% = `A4`, 100% = `FF`

Interference rejection (offset 34):
| Value | Setting |
|-------|---------|
| 0 | Off |
| 1 | Low |
| 2 | Medium |
| 3 | High |

Target expansion (offset 38):
| Value | Setting |
|-------|---------|
| 0 | Off |
| 1 | On |

Target boost (offset 42):
| Value | Setting |
|-------|---------|
| 0 | Off |
| 1 | Low |
| 2 | High |

Guard zones (offsets 54-88):
- **Per-radar**: Each radar (A/B) has independent guard zone settings
- **Sensitivity** (offset 54): 0-255, shared by both zones within same radar
- **Shape**: Determined by width field
  - Sector: width < 3600 (e.g., 68.3° = 683)
  - Cycle (full circle): width = 3599 (359.9°)
- **Range**: Inner/outer in meters (e.g., 18m inner, 33m outer)
- **Bearing**: Center angle in deci-degrees (e.g., 50° = 500)
- **Alarm mode** (enter/exit): NOT transmitted - chartplotter-internal logic

**Chartplotter-Internal Features (NOT in protocol):**
The following radar display features are computed/stored locally by the chartplotter
and are NOT transmitted in any radar report:
- Guard zone alarm mode (enter/exit trigger)
- Threshold setting (display threshold adjustment)
- Target trails (radar echo history/persistence)
- Acquire target (ARPA/MARPA target tracking)

### Report 03 - Model Info (129 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x03) |
| 1 | 1 | Command (0xC4) |
| 2 | 1 | Model byte |
| 3 | 31 | Unknown |
| 34 | 4 | Operating hours (total power-on time in hours) |
| 38 | 4 | Unknown (always 0x01) |
| 42 | 4 | Transmit seconds (total transmit time in seconds) |
| 46 | 12 | Unknown |
| 58 | 32 | Firmware date (UTF-16LE) |
| 90 | 32 | Firmware time (UTF-16LE) |
| 122 | 7 | Unknown |

**Example values:**
- Operating hours at offset 34: `81 0B 00 00` = 2945 hours
- Transmit seconds at offset 42: `60 2C 0A 00` = 666,720 seconds = 185.2 hours

Model bytes:
| Value | Model |
|-------|-------|
| 0x00 | HALO |
| 0x01 | 4G |
| 0x08 | 3G |
| 0x0E, 0x0F | BR24 |

### Report 04 - Installation (66 bytes)

Settings are per-radar (A/B can have different values on dual-range radars).

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x04) |
| 1 | 1 | Command (0xC4) |
| 2 | 4 | Unknown (always 0) |
| 6 | 2 | Bearing alignment (deci-degrees, i16, -1800 to +1799) |
| 8 | 2 | Unknown (always 0) |
| 10 | 2 | Antenna height (decimeters, u16) |
| 12 | 7 | Unknown (always 0) |
| 19 | 1 | Accent light (HALO only, 0-3) |
| 20 | 6 | Unknown (always 0) |
| 26 | 4 | Unknown per-radar value (u32, differs A vs B) |
| 30 | 4 | Unknown per-radar value (u32, differs A vs B) |
| 34 | 32 | Unknown (always 0) |

**Verified values (4G radar):**
- Antenna height 4m: offset 10-11 = `A0 0F` = 4000 mm
- Antenna height 10m: offset 10-11 = `10 27` = 10000 mm
- Bearing alignment 0°: offset 6-7 = `00 00` = 0
- Bearing alignment +90°: offset 6-7 = `84 03` = 900 deci-degrees
- Bearing alignment -123°: offset 6-7 = `42 09` = 2370 deci-degrees (= 237° = 360-123)

**Note:** Bearing alignment uses unsigned 0-3599 range. Negative values are
represented as 360° - |value|. For example, -123° is stored as 237° (2370).

**Observed per-radar values at offsets 26-33 (4G):**
- Radar A: offset 26 = 20, offset 30 = 180
- Radar B: offset 26 = 10, offset 30 = 10
- These values do NOT correspond to X-Axis/Y-Axis antenna position settings
- Purpose unknown (possibly timing, tuning, or guard zone parameters)

**Note:** The chartplotter has X-Axis and Y-Axis antenna position settings
(offset from ship center, supports positive/negative values). These are
**NOT transmitted in Report 04** - they may be chartplotter-internal only
or stored in a different report.

### Report 06 - Blanking Zones (68 or 74 bytes)

Contains radar name and up to 4 no-transmit zone definitions.

Each zone (5 bytes):
| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Enabled |
| 1 | 2 | Start angle (deci-degrees) |
| 3 | 2 | End angle (deci-degrees) |

### Report 08 - Advanced Settings (18/21/22 bytes)

Settings are per-radar (A/B have independent values on dual-range radars).

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

**Verified values (4G radar):**

Sea state (offset 2):
| Value | Setting |
|-------|---------|
| 0 | Calm |
| 1 | Moderate |
| 2 | Rough |

Local interference rejection (offset 3):
| Value | Setting |
|-------|---------|
| 0 | Off |
| 1 | Low |
| 2 | Medium |
| 3 | High |

Scan speed (offset 4):
| Value | Setting |
|-------|---------|
| 0 | Off (normal) |
| 1 | Medium |
| 2 | Medium-High |

Sidelobe suppression:
- Auto=off, 37%: offset 5 = `00`, offset 9 = `5F` (95 → 37.3%)
- Auto=off, 100%: offset 5 = `00`, offset 9 = `FF` (255 → 100%)
- Auto=on: offset 5 = `01`, offset 9 = current auto value
- **Offset 5**: `00` = manual, `01` = auto
- **Offset 9**: 0-255 value, formula: **percentage = value × 100 / 255**

Noise rejection (offset 12):
| Value | Setting |
|-------|---------|
| 0 | Off |
| 1 | Low |
| 2 | Medium |
| 3 | High |

Target separation (offset 13):
| Value | Setting |
|-------|---------|
| 0 | Off |
| 1 | Low |
| 2 | Medium |
| 3 | High |

**Note:** Threshold setting appears to be chartplotter-internal only.

### Report 07 - Statistics/Diagnostics (188 bytes)

Discovered on 4G radar. Contains mostly zeros with data at specific offsets.

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x07) |
| 1 | 1 | Command (0xC4) |
| 2 | 67 | Unknown (zeros) |
| 69 | 1 | Unknown (0x40 = 64 observed) |
| 70 | 66 | Unknown (zeros) |
| 136 | 4 | Counter/statistic 1 (u32, ~442778 observed) |
| 140 | 4 | Counter/statistic 2 (u32, ~238698 observed) |
| 144 | 4 | Counter/statistic 3 (u32, ~18415 observed) |
| 148 | 4 | Unknown (40 observed) |
| 152 | 4 | Per-radar value (A=45, B=40 observed) |
| 156 | 4 | Per-radar value (A=45, B=40 observed) |
| 160 | 4 | Unknown (20 observed) |
| 164 | 24 | Unknown (zeros) |

**Note:** The counter values at 136-147 may be related to packet counts or
timing statistics. Values at 152-159 differ between Radar A and B.

### Report 09 - Unknown (13 bytes)

Purpose unknown. May contain tuning or calibration indices.

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Type (0x09) |
| 1 | 1 | Command (0xC4) |
| 2 | 2 | Value 1 (u16, observed: 1) |
| 4 | 2 | Value 2 (u16, observed: 1) |
| 6 | 2 | Value 3 (u16, observed: 2) |
| 8 | 2 | Value 4 (u16, observed: 4) |
| 10 | 2 | Value 5 (u16, observed: 0) |
| 12 | 1 | Unknown (0) |

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

## Implementation Notes

### Network Interface Binding

**Critical**: When sending commands to the radar, the UDP socket must be bound to the
correct network interface (NIC). Navico radars are typically on a dedicated network
segment (e.g., 10.56.0.x). If the command socket is bound to `0.0.0.0` (any interface),
the operating system may route packets out the wrong interface, especially when:

- VPN is active (default route changes)
- Multiple network interfaces exist
- The radar network is not the default route

The solution is to bind the command socket to the specific NIC IP address where the
radar was discovered:

```rust
// Correct: bind to NIC address
socket.bind(&SocketAddr::new(nic_addr, 0))?;
socket.send_to(&command, radar_addr)?;

// Wrong: OS chooses interface, may pick wrong one
socket.bind(&SocketAddr::new(Ipv4Addr::UNSPECIFIED, 0))?;
```

The `nic_addr` is available from the radar discovery process - it's the local IP
address on which the radar beacon was received.

### Multi-NIC Multicast Configuration

**Critical for multi-NIC setups**: When a system has multiple network interfaces (e.g., WiFi
for internet and USB-Ethernet for radar), multicast groups must be joined on ALL interfaces
to ensure beacon reception regardless of which NIC the radar is connected to.

The problem occurs when joining multicast with `INADDR_ANY` (0.0.0.0) - the OS picks one
interface (typically the default route), which may not be the radar network.

```rust
// Wrong: OS picks one interface, often the wrong one
socket.join_multicast_v4(multicast_addr, Ipv4Addr::UNSPECIFIED)?;

// Correct: Join on each NIC explicitly
for nic_addr in &all_interface_addresses {
    socket.join_multicast_v4(multicast_addr, nic_addr)?;
}
```

Navico radars use link-local addressing (169.254.x.x) which is reachable from any connected
ethernet interface, so the code must discover all NICs at startup and join multicast groups
on each one.

### Linux Multicast Socket Configuration

**Critical for Linux**: When joining multicast groups, the `IP_MULTICAST_ALL` socket option
must be disabled. By default, Linux delivers multicast packets to ALL sockets that have
joined ANY multicast group, not just the specific group for that socket. This causes
beacon packets to be misrouted between different brand listeners (Navico, Raymarine, etc.).

```rust
// Linux requires disabling IP_MULTICAST_ALL for correct multicast reception
#[cfg(target_os = "linux")]
{
    use std::os::unix::io::AsRawFd;
    const IP_MULTICAST_ALL: libc::c_int = 49;

    let optval: libc::c_int = 0; // Disable
    libc::setsockopt(
        socket.as_raw_fd(),
        libc::SOL_IP,
        IP_MULTICAST_ALL,
        &optval as *const _ as *const libc::c_void,
        std::mem::size_of_val(&optval) as libc::socklen_t,
    );
}

// Then join the multicast group
socket.join_multicast_v4(multicast_addr, interface_addr)?;
```

See: https://man7.org/linux/man-pages/man7/ip.7.html (IP_MULTICAST_ALL)

### Link-Local Address Handling (169.254.x.x)

Navico radars typically use link-local IP addresses (169.254.x.x range, RFC 3927). These
addresses are auto-assigned by the radar and are valid only on the local network segment.

When determining which NIC to use for communication with a link-local radar:

1. **Don't rely on subnet matching** - link-local is not on any local subnet
2. **Prefer dedicated radar networks** - e.g., 172.31.x.x (Furuno/Navico shared subnet)
3. **Prefer wired interfaces** - avoid WiFi for radar data due to latency/reliability
4. **Track the receiving interface** - ideally, use the same NIC that received the beacon

```rust
fn find_nic_for_radar(radar_ip: &Ipv4Addr) -> Option<Ipv4Addr> {
    // Link-local special case
    if is_link_local(radar_ip) {
        // Prefer 172.31.x.x (dedicated radar network)
        if let Some(nic) = find_interface_on_subnet(172, 31) {
            return Some(nic);
        }
        // Fallback: prefer wired ethernet
        if let Some(nic) = find_wired_interface() {
            return Some(nic);
        }
    }
    // Normal subnet matching
    find_matching_subnet(radar_ip)
}
```

### Power Control String Values

The power/status control uses string enum values ("off", "standby", "transmit", "warming"),
not numeric values. When processing control updates, handle power specially before
attempting to parse as float:

```rust
// Handle power control first (string value, not numeric)
if control_id == "power" {
    let transmit = value.to_lowercase() == "transmit";
    send_power_command(transmit);
    return;
}

// Other controls use numeric values
let value: f32 = value.parse()?;
```

## References

- mayara-lib source: `src/brand/navico/`
- mayara-core protocol: `src/protocol/navico.rs`
- signalk-radar Go implementation: `radar-server/radar/navico/`
- OpenCPN radar_pi plugin (original reverse engineering)
- [Network captures from various Navico radar installations](https://github.com/keesverruijt/radar-recordings/tree/main/navico)
