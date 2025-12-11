# Raymarine Radar Protocol Documentation

This document describes the Raymarine radar network protocol as reverse-engineered from
network captures and the mayara-lib implementation.

## Supported Models

### RD Series (Magnetron/Analog)
- **RD418D**: 18" radome, 4kW
- **RD424D**: 24" radome, 4kW
- **RD418HD**: 18" radome, 4kW, HD (256-level)
- **RD424HD**: 24" radome, 4kW, HD (256-level)

### Open Array HD/SHD Series
- **Open Array HD 4kW** (E52069)
- **Open Array HD 12kW** (E92160)
- **Open Array SHD 4kW** (E52081)
- **Open Array SHD 12kW** (E52082)

### Magnum Series
- **Magnum 4kW** (E70484)
- **Magnum 12kW** (E70487)

### Quantum Series (Solid-State CHIRP)
- **Quantum Q24** (E70210): WiFi, no Doppler
- **Quantum Q24C** (E70344): Wired, no Doppler
- **Quantum Q24D** (E70498): Wired, with Doppler

### Cyclone Series (Next-gen Solid-State)
- **Cyclone** (E70620): With Doppler
- **Cyclone Pro** (E70621): With Doppler

## Network Architecture

Raymarine radars use UDP multicast for discovery and data transmission.

### Beacon Discovery

| Address | Port | Purpose |
|---------|------|---------|
| 224.0.0.1 | 5800 | Classic beacon address (RD/HD series) |
| 232.1.1.1 | varies | Quantum WiFi multicast |

### Two-Phase Discovery

Raymarine uses a two-beacon discovery mechanism:
1. **56-byte beacon**: Identifies the radar (link_id, model)
2. **36-byte beacon**: Provides endpoint addresses (same link_id)

Both beacons share a `link_id` field that correlates them.

## Radar Characteristics

| Model Type | Spokes | Spoke Length | Pixels | Doppler |
|------------|--------|--------------|--------|---------|
| RD (non-HD) | 2048 | 512/1024 | 16 (4-bit) | No |
| RD HD | 2048 | 1024 | 128 (7-bit) | No |
| Quantum | 250 | 252 | 128 (7-bit) | Q24D only |
| Cyclone | 250 | 252 | 128 (7-bit) | Yes |

### Pixel Data Format

**Non-HD (4-bit):**
- Values 0-15
- Packed 2 pixels per byte (low nibble, then high nibble)

**HD (8-bit with RLE):**
- Raw values 0-255, output 0-127 (shift right by 1)
- Uses run-length encoding with marker byte 0x5C

**Doppler (Quantum Q24D, Cyclone):**
- `0xFF` = Approaching target
- `0xFE` = Receding target

## Beacon Protocol

### 56-Byte Beacon (Identification)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Beacon type (0x00000001) |
| 4 | 4 | Subtype |
| 8 | 4 | Link ID |
| 12 | 4 | Unknown |
| 16 | 4 | Unknown |
| 20 | 32 | Model name (Quantum: "QuantumRadar", RD: empty) |
| 52 | 4 | Unknown |

Subtype values:
| Value | Model Type |
|-------|------------|
| 0x01 | RD/HD series |
| 0x66 | Quantum |
| 0x4D | Quantum Wireless |
| 0x11 | MFD request (ignore) |

### 36-Byte Beacon (Endpoints)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Beacon type (0x00000000) |
| 4 | 4 | Link ID |
| 8 | 4 | Subtype |
| 12 | 4 | Unknown |
| 16 | 4 | Unknown |
| 20 | 6 | Report/data address (little-endian) |
| 26 | 2 | Alignment |
| 28 | 6 | Command address (little-endian) |
| 34 | 2 | Alignment |

Subtype values:
| Value | Model Type |
|-------|------------|
| 0x01 | RD/HD series |
| 0x28 | Quantum |

### Little-Endian Socket Address (6 bytes)

```rust
struct LittleEndianSocketAddrV4 {
    addr: [u8; 4],  // IP as little-endian u32
    port: [u8; 2],  // Port as little-endian u16
}
```

IP conversion (little-endian to dotted decimal):
```rust
let ip_val = u32::from_le_bytes(addr);
let a = (ip_val >> 24) & 0xff;
let b = (ip_val >> 16) & 0xff;
let c = (ip_val >> 8) & 0xff;
let d = ip_val & 0xff;
// Result: a.b.c.d
```

### MFD Beacon Request (56 bytes)

Sent by MFDs to discover radars. Can be used to trigger radar responses:
```
01 00 00 00 11 00 00 00 38 8C 81 D4 6A 01 0E 83
6C 03 12 C6 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 02 00 01 00
```

## Report Protocol (UDP)

Reports are received on the report multicast address from the 36-byte beacon.

### Report Identification

All reports start with a 4-byte little-endian ID:

| ID | Model | Description |
|----|-------|-------------|
| 0x010001 | RD | Status report |
| 0x018801 | RD HD | Status report (HD variant) |
| 0x010002 | RD | Fixed/installation report |
| 0x010003 | RD | Spoke data frame |
| 0x010006 | RD | Info/model report |
| 0x280001 | Quantum | Info report |
| 0x280002 | Quantum | Status report |
| 0x280003 | Quantum | Spoke data frame |

## RD Series Protocol

### Info Report (0x010006)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Report ID (0x010006) |
| 4 | 7 | Serial number (ASCII) |
| ... | ... | Unknown |
| 20 | 7 | Model serial/part number |

Model identification by part number:
| Part Number | Model |
|-------------|-------|
| E92142 | RD418HD |
| E92143 | RD424HD |
| E92130 | RD418D |
| E92132 | RD424D |

### Status Report (0x010001 / 0x018801)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Report ID |
| 4 | 44 | Range table (11 × 4-byte values) |
| 48 | 132 | Unknown fields |
| 180 | 1 | Status |
| 181 | 3 | Unknown |
| 184 | 1 | Warmup time |
| 185 | 1 | Signal strength (bars) |
| 186 | 7 | Unknown |
| 193 | 1 | Range index |
| 194 | 2 | Unknown |
| 196 | 1 | Auto gain |
| 197 | 3 | Unknown |
| 200 | 4 | Gain value |
| 204 | 1 | Auto sea (0=off, 1=harbor, 2=offshore, 3=coastal) |
| 205 | 3 | Unknown |
| 208 | 1 | Sea value |
| 209 | 1 | Rain enabled |
| 210 | 3 | Unknown |
| 213 | 1 | Rain value |
| 214 | 1 | FTC enabled |
| 215 | 3 | Unknown |
| 218 | 1 | FTC value |
| 219 | 1 | Auto tune |
| 220 | 3 | Unknown |
| 223 | 1 | Tune value |
| 224 | 2 | Bearing offset (signed, degrees × 10) |
| 226 | 1 | Interference rejection |
| 227 | 3 | Unknown |
| 230 | 1 | Target expansion |
| 231 | 13 | Unknown |
| 244 | 1 | Main bang suppression enabled |

HD variant (0x018801) has range index at offset 296.

Status values:
| Value | Status |
|-------|--------|
| 0x00 | Standby |
| 0x01 | Transmit |
| 0x02 | Preparing/Warming |
| 0x03 | Off |

### Fixed Report (0x010002)

Contains installation/fixed settings. Data starts at offset 217:

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 2 | Magnetron operating time |
| 2 | 6 | Unknown |
| 8 | 1 | Magnetron current |
| 9 | 11 | Unknown |
| 20 | 2 | Rotation time (ignored, calculated) |
| 22 | 26 | Unknown |
| 48 | 1 | Display timing |
| 49 | 12 | Unknown |
| 61 | 1 | Unknown |
| 62 | 12 | Unknown |
| 74 | 1 | Gain min |
| 75 | 1 | Gain max |
| 76 | 1 | Sea min |
| 77 | 1 | Sea max |
| 78 | 1 | Rain min |
| 79 | 1 | Rain max |
| 80 | 1 | FTC min |
| 81 | 1 | FTC max |
| 82 | 4 | Unknown |
| 86 | 1 | Signal strength value |
| 87 | 2 | Unknown |

### Spoke Frame (0x010003)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Frame ID (0x010003) |
| 4 | 4 | Unknown |
| 8 | 4 | Unknown (0x0000001C) |
| 12 | 4 | Number of spokes |
| 16 | 4 | Spoke count |
| 20 | 4 | Unknown |
| 24 | 4 | Unknown (0x00000001) |
| 28 | 4 | Type field (0xFFFFFFFF or 0x400) |

Following the frame header, each spoke has:

**Spoke Header 1 (40 bytes):**
| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Type (0x00000001) |
| 4 | 4 | Length (0x00000028) |
| 8 | 4 | Azimuth |
| 12 | 4 | Field (0x01 or 0x03 for HD) |
| 16 | 4 | Field (0x02) |
| 20 | 4 | Field (0x01 or 0x03 for HD) |
| 24 | 4 | Field (0x01 or 0x00 for HD) |
| 28 | 4 | Field (0x01F4 or 0x00 for HD) |
| 32 | 4 | Zero |
| 36 | 4 | Field (0x01) |

**Optional Spoke Header 2 (8 bytes, if field01=0x02):**
| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Type (0x00000002) |
| 4 | 4 | Length |

**Spoke Data Header (12 bytes):**
| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Type (0x00000003 or 0x80000003) |
| 4 | 4 | Total length |
| 8 | 4 | Data length |

### Run-Length Encoding

RD HD and Quantum use RLE with marker byte 0x5C:

```
0x5C CC VV
```
- `0x5C` = Marker byte
- `CC` = Count (repeat count)
- `VV` = Value to repeat

Decoding:
```rust
if byte != 0x5C {
    output.push(byte >> 1);  // HD: shift right
} else {
    let count = next_byte;
    let value = next_byte >> 1;
    for _ in 0..count {
        output.push(value);
    }
}
```

## Quantum Series Protocol

### Info Report (0x280001)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Report ID (0x280001) |
| 4 | 6 | Model serial/part number |
| 10 | 7 | Serial number (ASCII) |

### Status Report (0x280002, 260 bytes)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Report ID (0x280002) |
| 4 | 1 | Status |
| 5 | 9 | Unknown |
| 14 | 2 | Bearing offset |
| 16 | 1 | Unknown |
| 17 | 1 | Interference rejection |
| 18 | 2 | Unknown |
| 20 | 1 | Range index |
| 21 | 1 | Mode (0=Harbor, 1=Coastal, 2=Offshore, 3=Weather) |
| 22 | 32 | Controls per mode (4 modes × 8 bytes) |
| 54 | 1 | Target expansion |
| 55 | 1 | Unknown |
| 56 | 3 | Unknown |
| 59 | 1 | Main bang suppression enabled |
| 60 | 88 | Unknown |
| 148 | 80 | Range table (20 × 4-byte values) |
| 228 | 32 | Unknown |

Controls per mode (8 bytes each):
| Offset | Size | Description |
|--------|------|-------------|
| 0 | 1 | Gain auto |
| 1 | 1 | Gain value |
| 2 | 1 | Color gain auto |
| 3 | 1 | Color gain value |
| 4 | 1 | Sea auto |
| 5 | 1 | Sea value |
| 6 | 1 | Rain enabled |
| 7 | 1 | Rain value |

### Spoke Frame (0x280003)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Report ID (0x280003) |
| 4 | 2 | Sequence number |
| 6 | 2 | Unknown (0x0101) |
| 8 | 2 | Scan length (returns per line) |
| 10 | 2 | Number of spokes (0x00FA = 250) |
| 12 | 2 | Unknown (0x0008) |
| 14 | 2 | Returns per range |
| 16 | 2 | Azimuth |
| 18 | 2 | Data length |
| 20 | ... | Spoke data (RLE encoded) |

Range calculation:
```rust
range = range_meters * returns_per_line / returns_per_range;
```

## Command Protocol (UDP)

Commands are sent to the command address from the 36-byte beacon.

### Stay-Alive Messages

Raymarine radars require periodic keep-alive messages to maintain connection.

#### RD/E120 Series Stay-Alive

**1-Second Interval (12 bytes):**
```
00 80 01 00 52 41 44 41 52 00 00 00
         R  A  D  A  R
```
Contains the ASCII string "RADAR" at offset 4.

**5-Second Interval (36 bytes):**
```
03 89 01 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 68 01 00 00
9E 03 00 00 B4 00 00 00 00 00 00 00
```

#### Quantum Series Stay-Alive

**1-Second Interval (12 bytes):**
```
00 00 28 00 52 61 64 61 72 00 00 00
         R  a  d  a  r
```
Note: Quantum uses lowercase "Radar" vs uppercase "RADAR" for E120.

**5-Second Interval (36 bytes):**
```
03 89 28 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00
9E 03 00 00 B4 00 00 00 00 00 00 00
```
Note: Third byte is `0x28` for Quantum vs `0x01` for E120.

#### Stay-Alive Timing

| Message | Interval | Purpose |
|---------|----------|---------|
| 1-second | Every 1s | Primary keep-alive |
| 5-second | Every 5s | Extended status/config |

Missing keep-alive messages may cause the radar to stop sending spoke data.

### RD Series Commands

Commands are variable-length, starting with a 2-byte opcode.

#### Status (Transmit/Standby)

```
01 80 01 00 VV 00 00 00
```
- VV: 0=standby, 1=transmit

#### Range

```
01 81 01 00 01 00 00 00 II 00 00 00
```
- II: Range index (0-based)

#### Bearing Alignment

```
07 82 01 00 VV VV VV VV
```
- VV VV VV VV: Deci-degrees as little-endian i32

#### Gain

Auto mode:
```
01 83 01 00 01 00 00 00 01 00 00 00 00 00 00 00 AA 00 00 00 00 00 00 00
```
- AA: 0=manual, 1=auto

Manual value:
```
01 83 01 00 01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 VV 00 00 00
```
- VV: Value (0-255)

#### Sea Clutter

Auto mode:
```
02 83 01 00 01 00 00 00 01 00 00 00 00 00 00 00 AA 00 00 00 00 00 00 00
```

Manual value:
```
02 83 01 00 01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 VV 00 00 00
```

#### Rain Clutter

```
03 83 01 00 01 00 00 00 01 00 00 00 00 00 00 00 AA 00 00 00 00 00 00 00
03 83 01 00 01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 VV 00 00 00
```

#### FTC (Fast Time Constant)

```
04 83 01 00 01 00 00 00 01 00 00 00 00 00 00 00 AA 00 00 00 00 00 00 00
04 83 01 00 01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 VV 00 00 00
```

#### Main Bang Suppression

```
01 82 01 00 01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 VV 00 00 00
```
- VV: 0=off, 1=on

#### Display Timing

```
02 82 01 00 01 00 00 00 VV 00 00 00
```

#### Interference Rejection

```
07 83 01 00 VV 00 00 00
```
- VV: 0=off, 1=normal, 2=high

### Quantum Series Commands

Quantum uses a simpler command format with 6-byte or 10-byte messages.

#### Helper Functions

One-byte command:
```
OP OP 28 00 00 VV 00 00
```
- OP OP: Opcode (2 bytes)
- VV: Value

Two-byte command:
```
OP OP 28 00 VV VV 00 00
```
- VV VV: Value (little-endian u16)

#### Status (Transmit/Standby)

```
01 80 01 00 VV 00 00 00
```
- VV: 0=standby, 1=transmit

#### Range

```
01 01 28 00 00 II 00 00
```
- II: Range index

#### Gain

Auto mode:
```
01 03 28 00 00 AA 00 00
```
- AA: 0=manual, 1=auto

Manual value (when auto=0):
```
02 83 28 00 00 VV 00 00
```
- VV: Value (0-255, scaled from 0-100)

#### Color Gain (Quantum)

```
03 03 28 00 00 AA 00 00   # Auto
04 03 28 00 00 VV 00 00   # Value
```

#### Sea Clutter

```
05 03 28 00 00 AA 00 00   # Auto
06 03 28 00 00 VV 00 00   # Value
```

#### Rain Clutter

```
0B 03 28 00 00 EE 00 00   # Enabled
0C 03 28 00 00 VV 00 00   # Value
```
- EE: 0=disabled, 1=enabled

#### Target Expansion

```
0F 03 28 00 00 VV 00 00
```
- VV: 0=off, 1=on

#### Interference Rejection

```
11 03 28 00 00 VV 00 00
```
- VV: Level (0-5)

#### Mode

```
14 03 28 00 00 VV 00 00
```
- VV: 0=Harbor, 1=Coastal, 2=Offshore, 3=Weather

#### Bearing Alignment

```
01 04 28 00 VV VV 00 00
```
- VV VV: Deci-degrees as little-endian i16

## Value Scaling

### Percentage to Byte

Control values 0-100% are scaled to 0-255:
```rust
fn scale_100_to_byte(value: f32) -> u8 {
    (value * 255.0 / 100.0).clamp(0.0, 255.0) as u8
}
```

### Wire Scale Factors

Some controls have specific wire scale factors:
- Bearing alignment: × 1800 (with offset -1)
- Gain: × 100
- FTC: × 100
- Rotation speed: × 990 (0.1 RPM units)

## Model Detection

Model detection happens via the Info Report:

1. Parse part number from info report
2. Look up model characteristics:

| Part Number | Model | Base | HD | Doppler |
|-------------|-------|------|-----|---------|
| E70210 | Quantum Q24 | Quantum | Yes | No |
| E70344 | Quantum Q24C | Quantum | Yes | No |
| E70498 | Quantum Q24D | Quantum | Yes | Yes |
| E70620 | Cyclone | Quantum | Yes | Yes |
| E70621 | Cyclone Pro | Quantum | Yes | Yes |
| E70484 | Magnum 4kW | RD | Yes | No |
| E70487 | Magnum 12kW | RD | Yes | No |
| E52069 | Open Array HD 4kW | RD | Yes | No |
| E92160 | Open Array HD 12kW | RD | Yes | No |
| E52081 | Open Array SHD 4kW | RD | Yes | No |
| E52082 | Open Array SHD 12kW | RD | Yes | No |
| E92142 | RD418HD | RD | Yes | No |
| E92143 | RD424HD | RD | Yes | No |
| E92130 | RD418D | RD | Yes | No |
| E92132 | RD424D | RD | Yes | No |

## Range Tables

Ranges are received in the status report as a table of values in internal units.
Convert to meters:
```rust
let meters = (internal_value as f64 * 1.852) as i32;
```

### RD Series Ranges

11 range values (indices 0-10)

### Quantum Series Ranges

20 range values (indices 0-19)

## References

- mayara-lib source: `src/brand/raymarine/`
- mayara-core protocol: `src/protocol/raymarine.rs`
- OpenCPN radar_pi plugin
- Network captures from Raymarine radar installations
