# Protocol Debugger User Guide

The Protocol Debugger is a development tool for analyzing and reverse-engineering marine radar protocols. It captures network traffic, decodes protocol messages, and helps identify unknown protocol elements.

> **Important:** This feature is only available when built with `--features dev`.

---

## Important Limitations

### What We Can See

| Traffic | Visible | Why |
|---------|---------|-----|
| mayara-server → radar | ✅ Yes | Goes through our sockets |
| radar → mayara-server | ✅ Yes | Received by our sockets |
| chart plotter → radar | ❌ No | Direct connection, bypasses us |
| radar → chart plotter | ⚠️ Partial | Multicast traffic visible, TCP not visible |

**Why this matters:** When you press a button on your Garmin/Simrad/Furuno MFD, the command goes directly from the chart plotter to the radar. We don't see the command, but:
- For **multicast protocols** (Navico, Garmin, Raymarine): We see the radar's status broadcasts change
- For **Furuno (TCP)**: We poll the radar every 2 seconds and see the updated state in the response

### Capturing Chart Plotter Commands

To capture chart plotter commands directly, use `tcpdump`:

```bash
# Capture all radar traffic on interface eth0
sudo tcpdump -i eth0 -w radar-capture.pcap host 172.31.1.4

# Capture specific ports (Furuno control)
sudo tcpdump -i eth0 -w furuno.pcap 'host 172.31.1.4 and port 10000'

# Capture multicast groups (Navico)
sudo tcpdump -i eth0 -w navico.pcap 'multicast and net 236.6.7.0/24'
```

Then analyze in Wireshark with the radar protocol documentation.

---

## Getting Started

### Prerequisites

- mayara-server built with `--features dev`
- A radar connected (or `.mrr` recording for playback)

### Enable the Debugger

```bash
# Build with dev features
cd /home/dirk/dev/mayara-server
cargo build -p mayara-server --features dev
./target/debug/mayara-server
```

### Open the Debug Panel

1. Open `http://localhost:6502/` in your browser
2. Click the **Debug** icon (🔬) in the toolbar (only visible in dev mode)
3. The debug panel appears as a collapsible sidebar

---

## Debug Panel Overview

### Radar Status Cards

Shows all connected radars with:
- Brand and model
- Connection state (green=connected, yellow=connecting, red=error)
- IP address

### Event Timeline

Real-time scrollable list of network events:
- **Blue (SEND)**: Data sent (TCP/UDP)
- **Green (RECV)**: Data received
- **Orange (SOCK)**: Socket operations (connect, bind, close)
- **Purple (STATE)**: State changes
- **Red/Orange (UNK)**: Unknown/unparseable messages

Click an event to see details in the Packet View.

### Filtering

Use the filter controls to narrow down events:
- **Text filter**: Search in radar ID, brand, command IDs, and raw ASCII
- **Radar filter**: Show events from a specific radar (populated automatically)
- **Type filter**:
  - All Types
  - Data (network traffic)
  - Socket Ops (connect, bind, close)
  - State Changes
  - **Unknown Only** - Shows only unparsed messages (useful for reverse engineering)

### Stats Bar

The bottom stats bar shows:
- **Total**: All events since session start
- **Buffer**: Events currently in memory (max 10,000)
- **Data**: Network traffic events
- **State**: State change events
- **Unknown**: Unparsed messages (highlighted in orange if > 0)

### Packet View

When an event is selected:
- **Hex dump**: Raw bytes with offset
- **ASCII view**: Printable characters (dots for non-printable)
- **Decoded fields**: Parsed protocol structure

For unknown bytes, regions are highlighted as `[UNKNOWN - N bytes]`.

### State Change View

Shows before/after comparison when radar state changes:
- Which control changed (e.g., `gain`, `sea`, `rain`, `power`)
- Previous and new values
- Triggering event (if correlatable)

**Automatic State Detection:**

The debugger automatically tracks control values from protocol responses and emits STATE events when they change. This works for **all supported brands**:

**Furuno** (ASCII over TCP):

| Control | Command | What's Tracked |
|---------|---------|----------------|
| `gain` / `gainAuto` | N63 | Gain value and auto mode |
| `sea` / `seaAuto` | N64 | Sea clutter value and auto mode |
| `rain` / `rainAuto` | N65 | Rain clutter value and auto mode |
| `power` | N69 | Standby/transmit state |

**Navico** (Binary UDP multicast - Simrad, B&G, Lowrance):

| Control | Report | What's Tracked |
|---------|--------|----------------|
| `power` / `powerStr` | Status (0x01) | Power state (off/standby/warmup/transmit) |
| `gain` / `gainAuto` | Settings (0x02) | Gain value and auto mode |
| `sea` / `seaAuto` | Settings (0x02) | Sea clutter value and mode (manual/auto/calm/moderate/rough) |
| `rain` | Settings (0x02) | Rain clutter value |
| `interference` | Settings (0x02) | Interference rejection level (0-3) |
| `range` | Range (0x08) | Range in decimeters |

**Raymarine** (Binary UDP multicast - Quantum and RD series):

| Control | Source | What's Tracked |
|---------|--------|----------------|
| `power` / `powerStr` | Status packet | Power state (standby/transmit) |
| `gain` / `gainAuto` | Status packet | Gain value and auto mode |
| `sea` / `seaAuto` | Status packet | Sea clutter value and auto mode |
| `rain` | Status packet | Rain clutter value |

**Garmin** (Binary UDP multicast - xHD series):

| Control | Packet Type | What's Tracked |
|---------|-------------|----------------|
| `power` / `powerStr` | 0x0919 | Power state (standby/transmit) |
| `gain` / `gainAuto` | 0x0924/0x0925 | Gain mode and value |
| `sea` / `seaAuto` | 0x0939/0x093a | Sea mode and value |
| `rain` / `rainAuto` | 0x0933/0x0934 | Rain mode and value |
| `range` | 0x091e | Range in meters |

**Unknown Command Tracking (Furuno only):**

For reverse engineering, the debugger also tracks state changes for **unknown Furuno commands**. When an unrecognized `$N` response changes, you'll see a STATE event like:

```
N68: ["1","0","50"] → ["1","0","75"]
```

This helps identify which parameter changed when you interact with the chart plotter, even for commands we don't yet understand.

---

## REST API

The debug feature adds these endpoints:

### WebSocket: Real-time Events
```
GET /v2/api/debug
```

Connect via WebSocket to receive real-time events.

**Client→Server messages:**
```json
{"type": "subscribe", "radarId": "radar-1"}  // Filter by radar
{"type": "getHistory", "limit": 100}         // Get historical events
{"type": "pause"}                             // Pause streaming
{"type": "resume"}                            // Resume streaming
```

**Server→Client messages:**
```json
{"type": "connected", "eventCount": 1234}
{"type": "event", ...}
{"type": "history", "events": [...]}
```

### Query Events
```
GET /v2/api/debug/events?radar_id=radar-1&limit=100&after=500
```

Returns historical events with optional filtering.

### Recording Control
```
POST /v2/api/debug/recording/start
Body: {"radars": [{"radarId": "radar-1", "brand": "furuno"}]}

POST /v2/api/debug/recording/stop

GET /v2/api/debug/recordings
```

---

## Workflow: Discovering Unknown Protocol Elements

### Step 1: Start Observing

1. Open the debug panel
2. Start mayara-server with a radar connected
3. Observe the initial handshake and status messages

### Step 2: Trigger Actions on Chart Plotter

1. Press a button on your chart plotter (e.g., change gain)
2. Watch the Event Timeline for new messages
3. Note the timestamp

### Step 3: Correlate Changes

When you change a setting on the chart plotter, the debugger shows:

1. **RECV event** - The radar's response containing the new state
2. **STATE event** - Automatic diff showing what changed

For example, changing gain from 50 to 75:
```
[14:30:45.123] RECV  $N63,0,75,0,...     ← Response with new value
[14:30:45.124] STATE gain: 50 → 75       ← Automatic state change detection
```

**For unknown commands**, you'll still see STATE events with raw parameters:
```
[14:30:46.500] RECV  $N68,1,0,75,...     ← Unknown command
[14:30:46.501] STATE N68: ["1","0","50"] → ["1","0","75"]
```

This makes it easy to identify which parameter changed, even for commands we don't yet understand.

**Note:** To see the actual command sent by the chart plotter (not just the radar's response), use `tcpdump`.

### Step 4: Record and Export

1. Click **Start Recording** in the debug panel
2. Perform actions on the chart plotter
3. Click **Stop Recording**
4. Add annotations (e.g., "Pressed Bird Mode at 14:30:45")
5. Export as `.mdbg` file for sharing

---

## Using tcpdump for Full Traffic Capture

Since the debugger can't see chart plotter → radar traffic directly, use `tcpdump`:

### Furuno (TCP on 172.31.x.x)

```bash
# Find radar IP
ip neigh | grep 172.31

# Capture all traffic to/from radar
sudo tcpdump -i eth0 -w furuno-session.pcap host 172.31.1.4

# In another terminal, use your chart plotter
# When done, Ctrl+C to stop capture

# Analyze in Wireshark
wireshark furuno-session.pcap
```

### Navico (UDP Multicast)

```bash
# Capture all Navico multicast traffic
sudo tcpdump -i eth0 -w navico.pcap 'multicast and (net 236.6.7.0/24 or net 239.238.55.0/24)'
```

### Raymarine (UDP Multicast)

```bash
# Capture Raymarine traffic
sudo tcpdump -i eth0 -w raymarine.pcap 'multicast and net 224.0.0.0/4 and port 5800'
```

### Garmin (UDP Multicast)

```bash
# Capture Garmin traffic
sudo tcpdump -i eth0 -w garmin.pcap 'multicast and net 239.254.2.0/24'
```

---

## Session Recording Format

`.mdbg` files are JSON and contain:
- All debug events with timestamps
- Radar capabilities and state at recording time
- User annotations
- mayara-server version

Files can be loaded by any developer to replay and analyze.

### Recording Structure

```json
{
  "metadata": {
    "formatVersion": 1,
    "startTime": "2024-01-15T14:30:22Z",
    "endTime": "2024-01-15T14:35:45Z",
    "serverVersion": "0.6.0",
    "radars": [
      {"radarId": "radar-1", "brand": "furuno", "model": "DRS4D-NXT"}
    ],
    "eventCount": 1234,
    "annotations": [
      {"timestamp": 123456, "note": "Pressed bird mode button"}
    ]
  },
  "events": [...]
}
```

---

## Tips for Effective Reverse Engineering

1. **Start with known operations**: First observe commands you already understand
2. **One change at a time**: Change one setting, observe the result
3. **Watch for STATE events**: The purple STATE badges highlight exactly what changed
4. **Filter by "State Changes"**: Use the type filter to see only state changes - great for spotting patterns
5. **Track unknown commands**: Even unrecognized commands get STATE events showing parameter changes
6. **Document timestamps**: Note exactly when you press each button
7. **Combine tools**: Use Protocol Debugger + tcpdump together
8. **Share recordings**: Upload `.mdbg` files to issues for collaboration

**Example workflow for discovering a new command:**

1. Set type filter to "State Changes"
2. Press a button on the chart plotter
3. Watch for a STATE event with an `N##` control ID (unknown command)
4. Note which parameter position changed (e.g., 3rd element in the array)
5. Repeat with different values to understand the mapping
6. Document in the protocol documentation

---

## Troubleshooting

### "No debug events appearing"

- Verify `--features dev` was used at compile time
- Check that radars are connected and transmitting
- Look at the Radar Status card for connection state
- Check the browser console for WebSocket errors
- Try refreshing the page (the debug panel reconnects automatically)

### "Can't see chart plotter commands"

This is expected for direct commands. However:
- For Furuno: State changes are visible via polling every 2 seconds
- For Navico/Raymarine/Garmin: Status broadcasts show the effect
- Use `tcpdump` to capture the actual commands

### "Decoded fields are empty"

The decoder may not recognize the message format. The raw hex is always available. Use the "Unknown Only" filter to find these messages. Consider contributing to the protocol documentation.

### "No STATE events appearing"

STATE events only appear after the **second** observation of a control value:
- First observation: Value is recorded internally (no event)
- Subsequent observations: If value differs, STATE event is emitted

This avoids flooding with "null → value" events on startup. To see STATE events:
1. Wait for the radar to report status at least once
2. Change a setting on the chart plotter
3. The next poll (within 2 seconds for Furuno) will show the STATE event

### "Radar not appearing in filter dropdown"

- Wait for some events to arrive (the dropdown populates from event data)
- Check that the radar is actually connected

### "Recording file is empty"

- Ensure you called "Start Recording" before performing actions
- Check that events were flowing during the recording period
- Verify disk space is available

---

## Protocol-Specific Notes

### Furuno

**Protocol Overview:**
- Uses ASCII commands over TCP (e.g., `$S69,50\r\n`)
- Commands start with `$S` (set), `$R` (request), `$N` (notification/response)
- State is polled every 2 seconds to sync changes from chart plotter
- You'll see `$R63` (request gain) followed by `$N63,auto,value,...` (response)

**Decoded Commands:**

| ID | Name | Description |
|----|------|-------------|
| 63 | Gain | `$N63,auto,value,...` - Gain level and auto mode |
| 64 | Sea | `$N64,auto,value,...` - Sea clutter and auto mode |
| 65 | Rain | `$N65,value,...` - Rain clutter level |
| 69 | Status | `$N69,mode,...` - Power state (1=standby, 2=transmit) |
| FF | Keepalive | `$SFF` / `$NFF` - Connection keepalive |

**Binary Framing:**

Some Furuno firmware versions wrap ASCII commands in an 8-byte binary header:
```
00 00 00 08 00 00 00 00 $N69,2,0,0,60,300,0
└──────────────────────┘└─────────────────┘
     8-byte header         ASCII command
```
The debugger automatically strips this header and decodes the ASCII command inside.

**Login Protocol:**

Furuno uses a binary login handshake before ASCII commands:

| Direction | Bytes | Description |
|-----------|-------|-------------|
| Send | 56 bytes starting `08 01 00 38` | Login request with copyright string |
| Recv | 12 bytes starting `09 01 00 0c` | Login response with command port offset |

The port offset (bytes 8-9) determines the command port: `10000 + offset`. For example, offset `0x0064` (100) means command port 10100.

**State Change Detection:**

The debugger automatically tracks known controls (gain, sea, rain, power) and emits STATE events when values change. For unknown commands, it tracks the raw parameters - useful for reverse engineering new commands.

### Navico (Simrad, B&G, Lowrance)

**Protocol Overview:**
- Uses binary UDP multicast
- Report types identified by first byte
- Spoke data is typically >100 bytes

**Decoded Reports:**

| Report ID | Name | What's Decoded |
|-----------|------|----------------|
| 0x01 | Status | Power state (off/standby/warmup/transmit) |
| 0x02 | Settings | Gain, sea, rain, interference rejection |
| 0x03 | Firmware | Firmware version info |
| 0x04 | Diagnostic | Bearing alignment |
| 0x08 | Range | Current range in decimeters |

**Field Offsets (Settings Report 0x02):**
- Gain auto: byte 11, Gain value: byte 12
- Sea value: byte 17, Sea auto: byte 21
- Rain value: byte 22
- Interference: byte 5

**State Change Detection:**

The debugger tracks all settings from the Settings report (0x02) and power state from Status report (0x01).

### Raymarine

**Protocol Overview:**
- Uses binary UDP multicast
- Two variants: Quantum (solid-state) and RD (magnetron)
- Beacon packets are 36-56 bytes

**Quantum Commands:**
- Format: `[opcode_lo, opcode_hi, 0x28, value, ...]`
- Status packets are 260+ bytes

| Opcode | Control |
|--------|---------|
| 0xc401 | Gain |
| 0xc402 | Sea clutter |
| 0xc403 | Rain clutter |
| 0xc404 | Range index |
| 0xc405 | Power |

**RD Commands:**
- Format: `[0x00, 0xC1, lead, value, 0x00, ...]`
- Status packets are 250-259 bytes

| Lead Byte | Control |
|-----------|---------|
| 0x01 | Gain |
| 0x02 | Sea clutter |
| 0x03 | Rain clutter |

**State Change Detection:**

The debugger tracks power, gain, sea, and rain from status packets for both Quantum and RD series.

### Garmin

**Protocol Overview:**
- Uses binary UDP multicast on 239.254.2.x
- 12-byte command packets (sent)
- Status packets are 8-100 bytes with packet type in first 4 bytes

**Decoded Packet Types:**

| Type Code | Name | What's Decoded |
|-----------|------|----------------|
| 0x0919 | Transmit | Power state (standby/transmit) |
| 0x0924 | Gain Mode | Auto/manual mode |
| 0x0925 | Gain Value | Gain level |
| 0x0939 | Sea Mode | Auto/manual mode |
| 0x093a | Sea Value | Sea clutter level |
| 0x0933 | Rain Mode | Auto/manual mode |
| 0x0934 | Rain Value | Rain clutter level |
| 0x091e | Range | Range in meters |

**Packet Format:**
- Bytes 0-3: Packet type (u32 LE)
- Bytes 4-7: Value (u32 LE)

**State Change Detection:**

The debugger tracks all decoded status packet types, including power, gain, sea, rain, and range.

---

## See Also

- [Getting Started](../develop/getting_started.md) - Development environment setup
- [Building](../develop/building.md) - Build commands and feature flags
- [Architecture](../design/architecture.md) - System design overview
