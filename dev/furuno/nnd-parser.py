#!/usr/bin/env python3
"""
Furuno NND (NavNet Demo) file parser.

Parses .nnd and .nnd.gz files from Furuno TZtouch demo recordings,
extracts IMO echo frames, decodes spoke headers and compressed echo
data, and prints per-spoke diagnostics.

Usage:
    python3 nnd-parser.py <file.nnd[.gz]> [--spokes] [--nmea] [--summary]

Options:
    --spokes   Print every spoke angle and pixel stats (verbose)
    --nmea     Print NMEA sentences
    --summary  Print per-frame summary only (default)
    --dup [N]  Dump the Nth duplicate angle pair (default: 1)
    --all      Enable all output
"""

import gzip
import math
import re
import struct
import sys
from collections import Counter
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Furuno protocol constants
# ---------------------------------------------------------------------------

SPOKES = 8192
SPOKE_ANGLE_MASK = 0x1FFF  # 13-bit angle

# Wire index -> meters (NM mode)
WIRE_INDEX_TABLE = {
    21: 116,     0: 231,     1: 463,     2: 926,     3: 1389,
    4: 1852,     5: 2778,    6: 3704,    7: 5556,    8: 7408,
    9: 11112,   10: 14816,  11: 22224,  12: 29632,  13: 44448,
    14: 59264,  19: 66672,  15: 88896,  20: 118528, 16: 133344,
    17: 177792, 18: 222240,
}


# ---------------------------------------------------------------------------
# IMO echo decoders
# ---------------------------------------------------------------------------

def decode_mode0(src, n):
    """Raw uncompressed."""
    return list(src[:n]), n


def decode_mode1(src, n):
    """Intra-spoke RLE, 7-bit pixels."""
    dst = []
    last = 0
    s = 0
    while len(dst) < n and s < len(src):
        b = src[s]
        s += 1
        if (b & 1) == 0:
            last = b & 0xFE
            dst.append(last)
        else:
            run = b >> 1
            if run == 0:
                run = 128
            dst.extend([last] * min(run, n - len(dst)))
    dst.extend([0] * (n - len(dst)))
    # Round consumed bytes up to multiple of 4
    return dst[:n], (s + 3) & ~3


def decode_mode2(src, prev, n):
    """Copy-from-prev-sweep, 7-bit pixels."""
    dst = []
    s = 0
    p = 0
    while len(dst) < n and s < len(src):
        b = src[s]
        s += 1
        if (b & 1) == 1:
            run = b >> 1
            if run == 0:
                run = 128
            for _ in range(min(run, n - len(dst))):
                val = prev[p] if p < len(prev) else 0
                dst.append(val)
                p += 1
        else:
            dst.append(b & 0xFE)
            p += 1
    dst.extend([0] * (n - len(dst)))
    return dst[:n], (s + 3) & ~3


def decode_mode3(src, prev, n):
    """2-bit marker, 6-bit pixels."""
    dst = []
    last = 0
    s = 0
    p = 0
    while len(dst) < n and s < len(src):
        b = src[s]
        s += 1
        marker = b & 3
        if marker == 0:
            last = b & 0xFC
            dst.append(last)
            p += 1
        else:
            run = b >> 2
            if run == 0:
                run = 64
            run = min(run, n - len(dst))
            if marker == 2:
                for _ in range(run):
                    last = prev[p] if p < len(prev) else 0
                    p += 1
                    dst.append(last)
            else:
                for _ in range(run):
                    dst.append(last)
                    p += 1
    dst.extend([0] * (n - len(dst)))
    return dst[:n], (s + 3) & ~3


# ---------------------------------------------------------------------------
# IMO frame header
# ---------------------------------------------------------------------------

@dataclass
class FrameHeader:
    magic: int
    sequence: int
    total_length: int
    timestamp: int
    spoke_data_len: int
    spoke_count: int
    sample_count: int
    encoding: int
    heading_valid: bool
    range_index: int
    range_status: int
    scale: int
    echo_type: int
    dual_range_id: int
    source: int

    @property
    def range_meters(self):
        return WIRE_INDEX_TABLE.get(self.range_index)

    @property
    def range_display(self):
        m = self.range_meters
        if m is None:
            return f"idx={self.range_index}"
        if m >= 1852:
            return f"{m / 1852:.1f} nm"
        return f"{m} m"


def parse_frame_header(data):
    """Parse the 16-byte IMO frame header."""
    if len(data) < 16 or data[0] != 0x02:
        return None

    b = data
    total_length = (b[2] << 8) | b[3]
    timestamp = b[4] | (b[5] << 8) | (b[6] << 16) | (b[7] << 24)
    spoke_data_len = ((b[9] & 0x01) << 8 | b[8]) * 4 + 4
    spoke_count = b[9] >> 1
    sample_count = (b[11] & 0x07) << 8 | b[10]
    encoding = (b[11] >> 3) & 0x03
    heading_valid = bool((b[11] >> 5) & 1)
    range_index = b[12] & 0x3F
    range_status = (b[12] >> 6) & 0x03
    scale = ((b[15] & 0x07) << 8) | b[14]
    echo_type = (b[15] >> 4) & 0x03
    dual_range_id = (b[15] >> 6) & 0x01
    source = b[15] >> 7

    return FrameHeader(
        magic=b[0], sequence=b[1], total_length=total_length,
        timestamp=timestamp, spoke_data_len=spoke_data_len,
        spoke_count=spoke_count, sample_count=sample_count,
        encoding=encoding, heading_valid=heading_valid,
        range_index=range_index, range_status=range_status,
        scale=scale, echo_type=echo_type,
        dual_range_id=dual_range_id, source=source,
    )


# ---------------------------------------------------------------------------
# Spoke extraction
# ---------------------------------------------------------------------------

@dataclass
class Spoke:
    angle: int
    heading: int
    pixels: list
    compressed: bytes  # raw compressed bytes before decoding
    nonzero_count: int
    max_pixel: int
    doppler_class: str  # "rain", "stationary", "moving", "mixed", "none"

    @property
    def angle_degrees(self):
        return self.angle * 360.0 / SPOKES


def classify_doppler(pixels):
    """Classify the doppler band of pixel values."""
    bands = Counter()
    for p in pixels:
        if p == 0:
            continue
        if p <= 60:
            bands["rain"] += 1
        elif 64 <= p <= 124:
            bands["stationary"] += 1
        elif 128 <= p <= 188:
            bands["moving"] += 1
        else:
            bands["unknown"] += 1
    if not bands:
        return "none"
    if len(bands) == 1:
        return list(bands.keys())[0]
    return "mixed"


def extract_spokes(data, header):
    """Extract all spokes from an IMO frame, returning list of Spoke."""
    spokes = []
    pos = 16  # skip frame header
    prev = [0] * header.sample_count
    n = header.sample_count

    for i in range(header.spoke_count):
        if pos + 4 > len(data):
            break

        angle = (data[pos] | ((data[pos + 1] & 0x1F) << 8)) & SPOKE_ANGLE_MASK
        heading = (data[pos + 2] | ((data[pos + 3] & 0x1F) << 8)) & SPOKE_ANGLE_MASK
        pos += 4

        remaining = data[pos:]
        if header.encoding == 0:
            pixels, used = decode_mode0(remaining, n)
        elif header.encoding == 1:
            pixels, used = decode_mode1(remaining, n)
        elif header.encoding == 2:
            if i == 0:
                pixels, used = decode_mode1(remaining, n)
            else:
                pixels, used = decode_mode2(remaining, prev, n)
        elif header.encoding == 3:
            pixels, used = decode_mode3(remaining, prev, n)
        else:
            break

        compressed = bytes(remaining[:used])
        pos += used
        prev = pixels

        nonzero = sum(1 for p in pixels if p > 0)
        max_px = max(pixels) if pixels else 0
        doppler = classify_doppler(pixels)

        spokes.append(Spoke(
            angle=angle, heading=heading, pixels=pixels,
            compressed=compressed,
            nonzero_count=nonzero, max_pixel=max_px,
            doppler_class=doppler,
        ))

    return spokes


# ---------------------------------------------------------------------------
# NND file parser
# ---------------------------------------------------------------------------

def parse_nnd(data):
    """
    Parse NND format, yield (timestamp_ms, lan_port, payload) tuples.

    The stated record length includes the header (digits + whitespace +
    ``LANx:``) plus 2 bytes of framing. After the payload,
    ``FSD\\n<FSDNN3FILE>\\n`` follows as a record separator.
    """
    pos = 0
    current_ts = 0

    while pos < len(data):
        # Skip whitespace
        if data[pos:pos + 1] in (b"\n", b"\r"):
            pos += 1
            continue

        # FSD separator — skip FSD\n and <FSDNN3FILE>\n
        if data[pos:pos + 3] == b"FSD":
            nl = data.find(b"\n", pos)
            pos = nl + 1 if nl >= 0 else len(data)
            if pos < len(data) and data[pos:pos + 1] == b"<":
                nl = data.find(b"\n", pos)
                pos = nl + 1 if nl >= 0 else len(data)
            continue

        # Time: header
        if data[pos:pos + 5] == b"Time:":
            nl = data.find(b"\n", pos)
            line = data[pos:nl].decode("ascii", errors="replace")
            parts = line.split()
            if len(parts) >= 2:
                try:
                    current_ts = int(parts[1])
                except ValueError:
                    pass
            pos = nl + 1 if nl >= 0 else len(data)
            continue

        # Packet record: <length><space>LAN<port>:<payload>
        m = re.match(rb"(\d+)\s+LAN(\d+):", data[pos:pos + 30])
        if m:
            stated_len = int(m.group(1))
            port = int(m.group(2))
            header_len = m.end()  # length of "321   LAN3:" relative to pos
            payload_len = stated_len - header_len + 2
            payload_start = pos + header_len
            payload_end = payload_start + payload_len
            if payload_len < 0 or payload_end <= pos or payload_end > len(data):
                pos += header_len  # skip past the header, don't go backwards
                continue
            yield (current_ts, port, data[payload_start:payload_end])
            pos = payload_end
            continue

        # Unknown — skip line
        nl = data.find(b"\n", pos, pos + 200)
        pos = nl + 1 if nl >= 0 else pos + 1


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def _hexdump(data, prefix="", width=32):
    """Print a hex dump with offset, hex bytes, and ASCII."""
    for off in range(0, len(data), width):
        chunk = data[off:off + width]
        hex_part = " ".join(f"{b:02x}" for b in chunk)
        ascii_part = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        print(f"{prefix}{off:4d}: {hex_part:<{width * 3}}  {ascii_part}")


def _dump_spoke(label, spoke, header):
    """Dump full compressed and decompressed data for a spoke."""
    print(f"--- {label} spoke: angle={spoke.angle} heading={spoke.heading} "
          f"nonzero={spoke.nonzero_count}/{len(spoke.pixels)} "
          f"max={spoke.max_pixel} doppler={spoke.doppler_class}")

    print(f"  Compressed ({len(spoke.compressed)} bytes):")
    _hexdump(spoke.compressed, prefix="    ")

    # Show decompressed pixels as value list, 32 per line
    print(f"  Decompressed ({len(spoke.pixels)} samples):")
    for off in range(0, len(spoke.pixels), 32):
        chunk = spoke.pixels[off:off + 32]
        vals = " ".join(f"{v:3d}" for v in chunk)
        print(f"    {off:4d}: {vals}")

    # Show differences if both spokes have same length
    return spoke


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    filename = sys.argv[1]
    args = sys.argv[2:]
    flags = set(args)
    show_spokes = "--spokes" in flags or "--all" in flags
    show_nmea = "--nmea" in flags or "--all" in flags
    show_dup = "--dup" in flags
    dup_target = 1
    if show_dup:
        dup_idx = args.index("--dup")
        if dup_idx + 1 < len(args) and args[dup_idx + 1].isdigit():
            dup_target = int(args[dup_idx + 1])
    show_summary = "--summary" in flags or "--all" in flags or (not flags and not show_dup)

    # Load file
    if filename.endswith(".gz"):
        with gzip.open(filename, "rb") as f:
            data = f.read()
    else:
        with open(filename, "rb") as f:
            data = f.read()

    print(f"File: {filename} ({len(data)} bytes)")

    # Statistics
    frame_count = 0
    total_spokes = 0
    angles_seen = set()
    doppler_stats = Counter()
    encoding_stats = Counter()
    range_stats = Counter()
    lan_stats = Counter()
    nmea_count = 0
    prev_angle = -1
    prev_spoke = None
    prev_header = None
    dup_count = 0
    stop = False

    for ts_ms, port, payload in parse_nnd(data):
        if stop:
            break
        lan_stats[port] += 1

        # NMEA sentences (LAN5 with 8-byte header, printable ASCII content)
        if len(payload) > 9 and payload[8:9] in (b"$", b"!"):
            nmea_raw = payload[8:]
            if all(32 <= b < 128 or b in (10, 13) for b in nmea_raw):
                if show_nmea:
                    text = nmea_raw.decode("ascii", errors="replace").strip()
                    for line in text.split("\r\n"):
                        line = line.strip()
                        if line.startswith("$") or line.startswith("!"):
                            print(f"  [{ts_ms:6d} ms] NMEA: {line}")
                nmea_count += 1
                continue

        # IMO echo frames (magic 0x02)
        if len(payload) < 16 or payload[0] != 0x02:
            continue

        header = parse_frame_header(payload)
        if header is None:
            continue

        frame_count += 1
        encoding_stats[header.encoding] += 1
        range_stats[header.range_index] += 1

        spokes = extract_spokes(payload, header)
        total_spokes += len(spokes)

        range_str = (
            f"range={header.range_display}"
            if header.range_meters
            else f"range_idx={header.range_index}"
        )

        # Build compact angle list with duplicate markers
        angle_parts = []
        dups_in_frame = 0
        for spoke in spokes:
            angles_seen.add(spoke.angle)
            doppler_stats[spoke.doppler_class] += 1
            dup = spoke.angle == prev_angle
            if dup:
                dups_in_frame += 1
            angle_parts.append(f"{'*' if dup else ''}{spoke.angle}")

            if show_dup and dup and prev_spoke is not None:
                dup_count += 1
                if dup_count == dup_target:
                    print(f"=== DUPLICATE #{dup_count}: ANGLE {spoke.angle} ({spoke.angle_degrees:.1f} deg) ===")
                    print(f"Frame #{frame_count}, seq={header.sequence}, "
                          f"enc={header.encoding}, samples={header.sample_count}")
                    print()
                    _dump_spoke("FIRST ", prev_spoke, prev_header)
                    print()
                    _dump_spoke("SECOND", spoke, header)
                    stop = True
                    break

            prev_angle = spoke.angle
            prev_spoke = spoke
            prev_header = header

        if show_summary:
            dup_str = f" ({dups_in_frame} dups)" if dups_in_frame else ""
            print(
                f"[{ts_ms:6d} ms] Frame #{frame_count}: "
                f"seq={header.sequence} {len(spokes)} spokes "
                f"enc={header.encoding} samples={header.sample_count} "
                f"scale={header.scale} {range_str} "
                f"echo_type={header.echo_type} "
                f"dual_range={'B' if header.dual_range_id else 'A'}{dup_str}"
            )
            print(f"  angles: {' '.join(angle_parts)}")

        if show_spokes:
            for spoke in spokes:
                print(
                    f"  spoke angle={spoke.angle:5d} "
                    f"({spoke.angle_degrees:6.1f} deg) "
                    f"heading={spoke.heading:5d} "
                    f"nonzero={spoke.nonzero_count:4d}/{header.sample_count} "
                    f"max={spoke.max_pixel:3d} "
                    f"doppler={spoke.doppler_class}"
                )

    # Final statistics
    print()
    print("=" * 60)
    print(f"Frames:       {frame_count}")
    print(f"Total spokes: {total_spokes}")
    print(f"Unique angles:{len(angles_seen)}/{SPOKES}")
    print()
    print("LAN port distribution:")
    for port, count in sorted(lan_stats.items()):
        print(f"  LAN{port}: {count}")
    print()
    print("Encoding modes:")
    for enc, count in sorted(encoding_stats.items()):
        names = {0: "raw", 1: "RLE", 2: "RLE+prev", 3: "2bit+prev"}
        print(f"  mode {enc} ({names.get(enc, '?')}): {count}")
    print()
    print("Range distribution:")
    for idx, count in sorted(range_stats.items()):
        m = WIRE_INDEX_TABLE.get(idx)
        if m and m >= 1852:
            label = f"{m / 1852:.1f} nm"
        elif m:
            label = f"{m} m"
        else:
            label = "?"
        print(f"  idx={idx:2d} ({label:>10s}): {count}")
    print()
    print("Doppler classification:")
    for cls, count in doppler_stats.most_common():
        print(f"  {cls}: {count}")
    print()
    print(f"NMEA packets: {nmea_count}")


if __name__ == "__main__":
    main()
