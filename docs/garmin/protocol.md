# Garmin Radar Protocol Documentation

This document describes the Garmin radar network protocol as reverse-engineered from
network captures and the mayara-lib implementation.

## Supported Models

### xHD Series
- **GMR 18 xHD**: 18" radome, 4kW
- **GMR 24 xHD**: 24" radome, 4kW
- **GMR 18 HD+**: Enhanced HD radome
- **GMR 24 HD+**: Enhanced HD radome
- **GMR 424 xHD**: Open array, 4kW
- **GMR 624 xHD**: Open array, 6kW
- **GMR 1224 xHD**: Open array, 12kW
- **GMR 1226 xHD**: Open array, 12kW (higher power)
- **GMR 2524 xHD2**: Open array, 25kW

### Fantom Series (Solid-State)
- **GMR Fantom 18**: Solid-state, 18" radome
- **GMR Fantom 24**: Solid-state, 24" radome
- **GMR Fantom 54/56**: Solid-state open array

## Network Architecture

Garmin radars use a simple UDP multicast architecture with fixed addresses.

### Network Addresses

| Address | Port | Protocol | Direction | Purpose |
|---------|------|----------|-----------|---------|
| 239.254.2.0 | 50100 | UDP | Radar → | Status reports |
| 239.254.2.0 | 50102 | UDP | Radar → | Spoke data |
| (radar IP) | 50101 | UDP | → Radar | Commands |

### Discovery Mechanism

Unlike Navico, Raymarine, or Furuno, Garmin radars don't use a structured beacon packet.
Discovery is implicit: any packet received on the report multicast address (239.254.2.0:50100)
indicates a radar is present. The source IP address of that packet becomes the radar's
command address.

## Radar Characteristics

| Parameter | Value |
|-----------|-------|
| Spokes per revolution | 1440 |
| Maximum spoke length | 705 (xHD) / 1024 (mayara) |
| Pixel values | 255 (8-bit) |
| Data format | 8-bit unpacked (1 pixel/byte) |
| Doppler support | No |

Note: The signalk-radar implementation uses 705 bytes per spoke (MAX_SPOKE_LEN), while
mayara-core uses 1024. The actual data length varies and is specified in each packet.

## Report Protocol (UDP)

Reports are multicast on 239.254.2.0:50100.

### Report Packet Format

All reports follow a simple structure:

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Packet type (little-endian u32) |
| 4 | 4 | Data length (little-endian u32) |
| 8 | len | Data (1, 2, or 4 bytes typically) |

### Report Types

| Type (hex) | Name | Length | Description |
|------------|------|--------|-------------|
| 0x0916 | ScanSpeed | 4 | Antenna rotation speed |
| 0x0919 | TransmitState | 4 | Radar transmit state |
| 0x091d | AutogainLevel | 4 | Auto gain level (Low=0, High=1) |
| 0x091e | Range | 4 | Range in meters |
| 0x0924 | AutogainMode | 4 | Gain mode (Manual=0, Auto=2) |
| 0x0925 | GainValue | 4 | Gain value |
| 0x0930 | BearingAlignment | 4 | Bearing offset (value/32 = degrees) |
| 0x0932 | CrosstalkRejection | 4 | Crosstalk rejection level |
| 0x0933 | RainClutterMode | 4 | Rain clutter mode |
| 0x0934 | RainClutterLevel | 4 | Rain clutter value |
| 0x0939 | SeaClutterMode | 4 | Sea clutter mode |
| 0x093a | SeaClutterLevel | 4 | Sea clutter value |
| 0x093b | SeaClutterAutoLevel | 4 | Sea clutter auto adjustment |
| 0x093f | NoTransmitZoneMode | 4 | NTZ enable/disable |
| 0x0940 | NoTransmitZoneStart | 4 | NTZ start (value/32 = degrees) |
| 0x0941 | NoTransmitZoneEnd | 4 | NTZ end (value/32 = degrees) |
| 0x0942 | TimedIdleMode | 4 | Timed idle mode |
| 0x0943 | TimedIdleTime | 4 | Idle time |
| 0x0944 | TimedIdleRunTime | 4 | Run time after idle |
| 0x0992 | ScannerStatus | 4 | Scanner operational status |
| 0x0993 | StatusChange | 4 | Time until status change (ms) |
| 0x099b | ScannerMessage | 80+ | Scanner info/model message |

### Transmit State Values

| Value | State |
|-------|-------|
| 0 | Off |
| 1 | Standby |
| 2 | Transmit |
| 3 | Warming Up |

### Gain Mode Values

| Value | Mode |
|-------|------|
| 0 | Manual |
| 2 | Auto |

### Gain Triplet

Garmin sends gain information across three separate packets in sequence every 2 seconds:

1. **0x0924** (AutogainMode): 0=manual, 2=auto
2. **0x0925** (GainValue): Current gain value
3. **0x091d** (AutogainLevel): 0=low, 1=high (only meaningful in auto mode)

**Interpretation:**
- Auto High: mode=2, level=1
- Auto Low: mode=2, level=0
- Manual: mode=0, level=(last used)

### Bearing Alignment

Bearing alignment values are encoded as:
```
encoded_value = degrees * 32
degrees = encoded_value / 32.0
```

Supports signed values for negative offsets.

### No Transmit Zone

NTZ angles use the same encoding as bearing alignment:
```
encoded_angle = degrees * 32
```

### Scanner Message (0x099b)

This packet contains model identification:

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Packet type (0x099b) |
| 4 | 4 | Data length (80+) |
| 8 | 16 | Unknown |
| 24 | 64 | Model info string (null-terminated) |

## Spoke Data Protocol (UDP)

Spoke data is multicast on 239.254.2.0:50102.

### Spoke Packet Format (from signalk-radar)

The detailed spoke packet structure (32-byte header):

```go
struct RadarLine {
    PacketType:       u32,    // Packet type identifier
    Len1:             u32,    // Packet length
    Fill_1:           u16,    // Padding
    ScanLength:       u16,    // Scan line length
    Angle:            u16,    // Spoke angle (divide by 8 for output)
    Fill_2:           u16,    // Padding
    RangeMeters:      u32,    // Range in meters
    DisplayMeters:    u32,    // Display range in meters
    Fill_3:           u16,    // Padding
    ScanLengthBytesS: u16,    // Scan data length (short format)
    Fill_4:           u16,    // Padding
    ScanLengthBytesI: u32,    // Scan data length (int format)
    Fill_5:           u16,    // Padding
    // followed by line_data payload
}
```

### Simplified Spoke Header (mayara-core)

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Packet type (typically 0x2904) |
| 4 | 4 | Data length (little-endian u32) |
| 8 | 2 | Bearing (raw value) |
| 10 | 4 | Range in meters |
| 14 | 2 | Unknown |
| 16 | ... | Spoke pixel data |

### Bearing Conversion

Garmin uses different scaling:
- signalk-radar: `spoke_angle = raw_angle / 8`
- mayara-core: `degrees = (raw_bearing / 4096.0) * 360.0`

Both yield the same result for 1440 spokes:
```
spoke_index = raw_bearing * 1440 / 4096
```

### Pixel Data Format

Garmin xHD uses 8-bit pixels (not 4-bit like Navico):

- 8 bits per pixel (values 0-255)
- 1 pixel per byte (no packing)
- Variable spoke length (up to 705 bytes)
- No Doppler encoding

Color mapping (255-level gradient):
- Pixel 0: Transparent (no signal)
- Pixels 1-255: Blue → Green → Red gradient (signal strength)

## Command Protocol (UDP)

Commands are sent to the radar's IP address on port 50101.

### Command Packet Format

Commands use the same format as reports:

| Offset | Size | Description |
|--------|------|-------------|
| 0 | 4 | Packet type (little-endian u32) |
| 4 | 4 | Data length (4) |
| 8 | 4 | Value (little-endian u32) |

### Command Types

Commands use the same packet types as reports:

| Type (hex) | Command | Value |
|------------|---------|-------|
| 0x0919 | Status | 1=standby, 2=transmit |
| 0x091e | Range | Range in meters |
| 0x0924 | Gain Mode | 0=manual, 2=auto |
| 0x0925 | Gain Value | Gain level |
| 0x0930 | Bearing Alignment | degrees × 32 |
| 0x0932 | Crosstalk Rejection | Level |
| 0x0933 | Rain Clutter Mode | 0=manual, 1=auto |
| 0x0934 | Rain Clutter Level | Value |
| 0x0939 | Sea Clutter Mode | 0=manual, 1=auto |
| 0x093a | Sea Clutter Level | Value |
| 0x093f | NTZ Mode | 0=off, 1=on |
| 0x0940 | NTZ Start | degrees × 32 |
| 0x0941 | NTZ End | degrees × 32 |

### Command Examples

**Transmit On:**
```
19 09 00 00 04 00 00 00 02 00 00 00
```

**Transmit Off (Standby):**
```
19 09 00 00 04 00 00 00 01 00 00 00
```

**Set Range to 5000m:**
```
1e 09 00 00 04 00 00 00 88 13 00 00
```

**Set Manual Gain to 50:**
```
24 09 00 00 04 00 00 00 00 00 00 00   # Mode = Manual (0)
25 09 00 00 04 00 00 00 32 00 00 00   # Value = 50
```

**Set Auto Gain (High):**
```
24 09 00 00 04 00 00 00 02 00 00 00   # Mode = Auto (2)
1d 09 00 00 04 00 00 00 01 00 00 00   # Level = High (1)
```

**Set Bearing Alignment to 10 degrees:**
```
30 09 00 00 04 00 00 00 40 01 00 00   # 320 = 10 × 32
```

**Enable No Transmit Zone (90° to 180°):**
```
3f 09 00 00 04 00 00 00 01 00 00 00   # Mode = On (1)
40 09 00 00 04 00 00 00 40 0b 00 00   # Start = 90 × 32 = 2880
41 09 00 00 04 00 00 00 80 16 00 00   # End = 180 × 32 = 5760
```

## Multi-Parameter Commands

Some controls require multiple packets sent together:

### Gain Control
```rust
// Set auto gain
create_command(0x0924, 2);  // mode = auto
create_command(0x0925, value);

// Set manual gain
create_command(0x0924, 0);  // mode = manual
create_command(0x0925, value);
```

### Sea Clutter Control
```rust
create_command(0x0939, mode);   // 0=manual, 1=auto
create_command(0x093a, value);
```

### Rain Clutter Control
```rust
create_command(0x0933, mode);   // 0=manual, 1=auto
create_command(0x0934, value);
```

### No Transmit Zone
```rust
create_command(0x093f, enabled);      // 0=off, 1=on
create_command(0x0940, start * 32);   // start angle
create_command(0x0941, end * 32);     // end angle
```

## Value Ranges

| Control | Range | Notes |
|---------|-------|-------|
| Gain | 0-255 | Internal value |
| Sea Clutter | 0-255 | Internal value |
| Rain Clutter | 0-255 | Internal value |
| Bearing Alignment | -180° to +180° | Encoded × 32 |
| NTZ Angles | 0° to 360° | Encoded × 32 |
| Range | Model-dependent | In meters |

## Protocol Characteristics

### Simplicity

Garmin's protocol is notably simpler than Navico, Raymarine, or Furuno:
- No structured beacon/discovery packets
- No login or session management
- No keep-alive requirements
- Single packet format for both reports and commands
- Fire-and-forget commands (no acknowledgment)

### Multicast Usage

- All radar traffic uses the same multicast group (239.254.2.0)
- Different ports distinguish report data (50100), spoke data (50102)
- Commands are unicast to the radar's IP on port 50101

### Report Timing

Reports are sent periodically by the radar:
- Status reports: Every few seconds
- Range/gain triplets: Every ~2 seconds
- Scanner messages: Periodically

## Implementation Notes

### Discovery

```rust
// Listen for any packet on report multicast
let report_addr = "239.254.2.0:50100";
let sock = join_multicast(report_addr)?;

// First packet received indicates radar presence
let (_, from) = sock.recv_from(&mut buf)?;

// Use source IP for commands
let command_addr = SocketAddr::new(from.ip(), 50101);
```

### Sending Commands

Commands are stateless UDP packets:

```rust
async fn send_command(&self, packet_type: u32, value: u32) -> Result<()> {
    let mut cmd = Vec::with_capacity(12);
    cmd.extend_from_slice(&packet_type.to_le_bytes());
    cmd.extend_from_slice(&4u32.to_le_bytes());
    cmd.extend_from_slice(&value.to_le_bytes());
    self.sock.send_to(&cmd, &self.radar_addr).await?;
    Ok(())
}
```

## Garmin HD (Legacy) Protocol

The older Garmin HD series (pre-xHD) uses different packet type codes while maintaining
the same basic packet structure.

### HD vs xHD Packet Type Comparison

| Function | HD Type | xHD Type | Notes |
|----------|---------|----------|-------|
| TX Off | 0x02B2 | 0x0919 | HD: parm1=1, xHD: parm1=0 |
| TX On | 0x02B2 | 0x0919 | HD: parm1=2, xHD: parm1=1 |
| Range | 0x02B3 | 0x091E | HD: meters-1, xHD: meters |
| Gain Mode | 0x02B4 | 0x0924 | HD: 344=auto, xHD: 0=manual/2=auto |
| Gain Value | 0x02B4 | 0x0925 | HD combined, xHD separate |
| Sea Clutter | 0x02B5 | 0x0939/093A | HD combined, xHD mode+value |
| Rain Clutter | 0x02B6 | 0x0933/0934 | HD combined, xHD mode+value |
| Bearing | 0x02B7 | 0x0930 | Same encoding (×32) |
| FTC | 0x02B8 | - | HD only |
| Interference | 0x02B9 | 0x0920 | Same values |
| Scan Speed | 0x02BE | 0x0932 | Same values |

### HD Packet Structures

HD uses packed C structs:

**9-byte packet (1-byte parameter):**
```c
struct rad_ctl_pkt_9 {
  uint32_t packet_type;  // e.g., 0x02B9
  uint32_t len1;         // Always 1
  uint8_t parm1;         // Value
};
```

**10-byte packet (2-byte parameter):**
```c
struct rad_ctl_pkt_10 {
  uint32_t packet_type;
  uint32_t len1;         // Always 2
  uint16_t parm1;        // Value (little-endian)
};
```

**12-byte packet (4-byte parameter):**
```c
struct rad_ctl_pkt_12 {
  uint32_t packet_type;
  uint32_t len1;         // Always 4
  uint32_t parm1;        // Value (little-endian)
};
```

### HD Gain Control

HD handles auto gain differently than xHD:

```
// Auto gain
packet_type = 0x02B4
parm1 = 344            // Magic value indicating auto mode

// Manual gain
packet_type = 0x02B4
parm1 = <value>        // 0-255 gain value
```

### HD Sea Clutter

HD sea clutter uses a multi-parameter packet:

```c
struct sea_clutter_pkt {
  uint32_t packet_type;  // 0x02B5
  uint32_t len1;
  uint16_t value;        // Sea clutter value
  uint16_t mode;         // 0=off, 1=calm, 2=medium/rough
  uint16_t parm3;        // Additional flag
  uint16_t parm4;        // Additional flag
};
```

### HD FTC (Fast Time Constant)

HD has FTC control not present in xHD:

```
packet_type = 0x02B8
parm1 = <value>        // FTC level
```

## Comparison with Other Brands

| Feature | Garmin xHD | Garmin HD | Navico | Raymarine |
|---------|-----------|-----------|--------|-----------|
| Spokes/revolution | 1440 | 1440 | 2048 | 2048/250 |
| Spoke resolution | 0.25° | 0.25° | 0.176° | 0.176°/1.44° |
| Pixel depth | 8-bit (255) | 8-bit (255) | 4-bit (16) | 7-bit (128) |
| Samples/spoke | ~705 | ~705 | 512 | 252-1024 |
| Doppler | No | No | HALO only | Q24D/Cyclone |
| Discovery | Implicit | Implicit | Beacon | Two-phase beacon |
| Keep-alive | None | None | Required | None |
| FTC Control | No | Yes | No | Yes |

## References

- mayara-lib source: `src/brand/garmin/`
- mayara-core protocol: `src/protocol/garmin.rs`
- signalk-radar Go implementation: `radar-server/radar/garminxhd/`
- OpenCPN radar_pi plugin (GarminHD and GarminxHD implementations)
- Network captures from Garmin xHD radar installations
- Sample PCAP: `signalk-radar/demo/samples/garmin_xhd.pcap`
