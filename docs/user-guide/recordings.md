# Recording and Playing Back Radar Data

This guide explains how to record radar data to `.mrr` files and play them back later.

## Overview

MaYaRa can record live radar data to files and play them back without a connected radar. This is useful for:

- **Demos and exhibitions** - Show radar functionality without hardware
- **Testing and development** - Debug with consistent, repeatable data
- **Sharing captures** - Send interesting radar recordings to others
- **Training** - Practice radar interpretation with recorded scenarios

## Requirements

- **mayara-server** running with a radar connected (for recording)
- **mayara-server** or **SignalK with playback plugin** (for playback)

---

## Recording Radar Data

### Step 1: Start mayara-server

Start mayara-server with your radar connected:

```bash
mayara-server
```

The server starts on port 6502 by default.

### Step 2: Open the Recordings Page

Open your browser and navigate to:

```
http://localhost:6502/recordings.html
```

### Step 3: Select a Radar

The **Record** section shows all connected radars. Select the radar you want to record.

### Step 4: Start Recording

1. Optionally enter a custom filename (default is timestamp-based)
2. Click **Start Recording**
3. The status shows "Recording..." with elapsed time and frame count

### Step 5: Stop Recording

Click **Stop Recording** when done. The recording appears in the files list.

**Tips for good recordings:**
- Record at least 30 seconds to capture a full antenna revolution
- Include interesting scenarios (weather, targets, etc.)
- Avoid changing radar settings during recording if you want consistent playback

---

## Managing Recording Files

### Viewing Files

The **Files** section lists all recordings with:
- Filename
- Duration
- File size
- Frame count
- Recording date

### Downloading

Click **Download** to get a compressed `.mrr.gz` file (~95% smaller than original).

### Deleting

Click **Delete** to remove a recording. This cannot be undone.

### Storage Location

Recordings are stored in:
- **Linux:** `~/.local/share/mayara/recordings/`
- **macOS:** `~/Library/Application Support/mayara/recordings/`
- **Windows:** `%APPDATA%\mayara\recordings\`

---

## Playing Back Recordings

### Method 1: mayara-server (Standalone)

Play recordings directly through mayara-server without any radar hardware.

#### Load a Recording

1. Open `http://localhost:6502/recordings.html`
2. Go to the **Playback** section
3. Select a recording from the list
4. Click **Load**

#### Control Playback

- **Play** - Start playback
- **Pause** - Pause at current position
- **Stop** - Stop and unload the recording
- **Loop** - Repeat when reaching the end

#### View the Radar

1. Click **View Radar** or go to `http://localhost:6502/`
2. The playback radar appears in the radar list as "Playback: {filename}"
3. Select it to view the radar display

**Note:** During playback, radar controls are disabled since you're viewing recorded data.

### Method 2: SignalK Playback Plugin

Play recordings through SignalK for integration with other SignalK applications.

#### Install the Plugin

1. Open SignalK server web interface
2. Go to **Appstore** > **Available**
3. Search for "MaYaRa Radar Playback"
4. Click **Install** and restart SignalK

#### Upload a Recording

1. Navigate to the playback plugin page
2. Drag and drop a `.mrr` or `.mrr.gz` file onto the upload zone
3. Or click to browse for a file

#### Play the Recording

1. Select the recording from the list
2. Click **Load**
3. Click **Play**
4. Click **View Radar** to see the display

#### Using with Other SignalK Clients

During playback, the recording registers as a virtual radar in SignalK. Any SignalK radar consumer can connect to it:

- The radar appears at `/signalk/v2/api/vessels/self/radars/playback-{filename}`
- Spoke data streams via SignalK's binary WebSocket

---

## File Format

### .mrr Files

MaYaRa Radar Recording files contain:
- Radar capabilities and settings at time of recording
- All spoke data with precise timestamps
- Control state changes during recording

**File sizes:** Approximately 15-30 MB per minute of recording.

### .mrr.gz Files

Gzip-compressed recordings for efficient transfer. About 95% smaller than uncompressed.

Both formats are supported for upload. Files are stored uncompressed for fast playback.

---

## Playback Behavior

### Virtual Radar

During playback, a "virtual radar" is created that behaves like a real radar:
- Appears in the radar list
- Streams spoke data at recorded timing
- Shows recorded capabilities and settings

### Disabled Controls

Radar controls are disabled during playback because:
- The data is pre-recorded and cannot be changed
- Settings reflect what was active during recording
- The "PLAYBACK" badge indicates you're viewing recorded data

### Timing

Playback runs at the original recording speed. Spokes are emitted at the same intervals they were recorded.

---

## Troubleshooting

### Recording Issues

**No radars available to record:**
- Check that mayara-server is running
- Verify the radar is connected and powered on
- Check network connectivity to the radar

**Recording stops unexpectedly:**
- Check available disk space
- Review mayara-server logs for errors

### Playback Issues

**Recording won't load:**
- Verify the file is a valid `.mrr` or `.mrr.gz` file
- Check for file corruption (try re-downloading)
- Review logs for parsing errors

**No radar appears during playback:**
- Make sure playback is started (not just loaded)
- Refresh the radar list in the viewer
- Check that the playback didn't finish (enable Loop)

**Radar shows "STANDBY" instead of spokes:**
- This indicates the playback radar's power status is not set correctly
- The playback radar should automatically report "transmit" status
- If you see STANDBY, the recording may have captured the radar in standby mode
- Try a different recording or ensure the radar was transmitting during recording

**WebSocket disconnects immediately (code 1006):**
- Check mayara-server logs for broadcast channel errors
- This can happen if the client can't keep up with high-rate spoke data
- The server handles "lagged" connections gracefully, but severe lag may cause disconnection
- Try reducing other network/CPU load on the viewing device

**Switching between playback files causes confusion:**
- When selecting a different file, the previous playback should stop automatically
- If you see spokes from the wrong recording, stop playback and reload the file
- Wait a moment between stopping one playback and loading another

**Playback stutters or skips:**
- Normal on slower systems with high-resolution recordings
- Close other applications to free resources
- The timeline slider allows seeking to any position if playback falls behind

---

## Command Line Options

mayara-server supports command-line options for recordings:

```bash
# Specify custom recordings directory
mayara-server --recordings-dir /path/to/recordings

# Start with a recording loaded (no playback yet)
mayara-server --load-recording filename.mrr
```

---

## Best Practices

### For Recording

1. **Name recordings descriptively** - Include date, location, or scenario
2. **Record complete revolutions** - At least 30-60 seconds
3. **Document settings** - Note the range, gain, and other settings used
4. **Test playback** - Verify the recording works before archiving

### For Sharing

1. **Use .mrr.gz format** - Much smaller for email/transfer
2. **Include metadata** - Add a text file describing the recording
3. **Test on clean system** - Verify playback works without your local setup

### For Development

1. **Create test fixtures** - Record specific scenarios for automated testing
2. **Version control** - Store small recordings in git for regression tests
3. **Document edge cases** - Record unusual radar behavior for debugging
