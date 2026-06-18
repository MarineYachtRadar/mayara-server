#!/usr/bin/env python3
"""
Spoke viewer for mayara-server.

Connects to a running mayara-server, discovers the first radar,
and displays sampled spoke data as ASCII art.

Usage:
    python3 spoke_viewer.py [--url http://localhost:6502] [--insecure]

Requirements:
    pip install websockets requests protobuf
"""

import argparse
import asyncio
import math
import ssl
import struct
import sys
import time
from pathlib import Path

import requests
import websockets

# ---------------------------------------------------------------------------
# Load the RadarMessage protobuf definition from the source tree
# ---------------------------------------------------------------------------

def compile_proto():
    """Compile RadarMessage.proto from the source tree and import it."""
    proto_path = Path(__file__).resolve().parent.parent.parent / "src" / "lib" / "protos"
    proto_file = proto_path / "RadarMessage.proto"
    if not proto_file.exists():
        sys.exit(f"Cannot find {proto_file}")

    # Compile into a temp directory so we don't pollute the source tree
    import tempfile
    out_dir = Path(tempfile.mkdtemp(prefix="mayara_proto_"))

    from google.protobuf.compiler import plugin_pb2  # noqa: F401 – just check protobuf is installed
    raise ImportError  # fall through to grpc_tools / shell compile


def load_proto():
    """
    Import the compiled protobuf module.
    Tries grpc_tools first, then shelling out to protoc.
    """
    proto_path = Path(__file__).resolve().parent.parent.parent / "src" / "lib" / "protos"
    proto_file = proto_path / "RadarMessage.proto"
    if not proto_file.exists():
        sys.exit(f"Cannot find {proto_file}")

    import tempfile, importlib
    out_dir = Path(tempfile.mkdtemp(prefix="mayara_proto_"))

    # Try grpc_tools
    try:
        from grpc_tools import protoc as grpc_protoc
        rc = grpc_protoc.main([
            "grpc_tools.protoc",
            f"--proto_path={proto_path}",
            f"--python_out={out_dir}",
            "RadarMessage.proto",
        ])
        if rc != 0:
            raise RuntimeError(f"grpc_tools.protoc returned {rc}")
    except (ImportError, RuntimeError):
        # Fall back to system protoc
        import subprocess
        try:
            subprocess.check_call([
                "protoc",
                f"--proto_path={proto_path}",
                f"--python_out={out_dir}",
                "RadarMessage.proto",
            ])
        except FileNotFoundError:
            sys.exit(
                "Cannot compile RadarMessage.proto.\n"
                "Install one of:\n"
                "  pip install grpcio-tools\n"
                "  brew install protobuf   (or apt install protobuf-compiler)\n"
            )

    sys.path.insert(0, str(out_dir))
    return importlib.import_module("RadarMessage_pb2")


pb2 = load_proto()

# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

verify_tls = True

def discover_radar(base_url):
    """Fetch the first radar from the API and return (radar_id, radar_info)."""
    r = requests.get(f"{base_url}/signalk/v2/api/vessels/self/radars", verify=verify_tls)
    r.raise_for_status()
    data = r.json()
    radars = data
    if not radars:
        sys.exit("No radars found. Is the server running with --emulator or a real radar?")
    radar_id = next(iter(radars))
    return radar_id, radars[radar_id]


def fetch_capabilities(base_url, radar_id):
    """Fetch radar capabilities."""
    r = requests.get(f"{base_url}/signalk/v2/api/vessels/self/radars/{radar_id}/capabilities", verify=verify_tls)
    r.raise_for_status()
    return r.json()

# ---------------------------------------------------------------------------
# ASCII art helpers
# ---------------------------------------------------------------------------

SHADE = " ·:+#@"  # 6 levels: nothing, faint, light, medium, strong, max

def spoke_to_ascii(data, width=64):
    """Convert spoke byte data to an ASCII string of the given width."""
    if not data:
        return " " * width
    step = max(1, len(data) / width)
    chars = []
    for i in range(width):
        idx = int(i * step)
        if idx >= len(data):
            break
        val = data[idx]
        level = min(len(SHADE) - 1, val * len(SHADE) // 64) if val > 0 else 0
        chars.append(SHADE[level])
    return "".join(chars).ljust(width)


def format_angle(angle, spokes_per_rev):
    """Format a spoke angle as degrees."""
    deg = angle * 360.0 / spokes_per_rev
    return f"{deg:6.1f}°"


def format_bearing(bearing, spokes_per_rev):
    """Format a bearing as compass degrees."""
    if bearing is None:
        return "  N/A "
    deg = bearing * 360.0 / spokes_per_rev
    return f"{deg:6.1f}°"


def format_range(meters):
    """Format range in human-readable units."""
    if meters >= 1852:
        return f"{meters / 1852:.1f} nm"
    return f"{meters} m"


def format_time(ms):
    """Format epoch millis as HH:MM:SS."""
    if ms is None:
        return "N/A"
    return time.strftime("%H:%M:%S", time.gmtime(ms / 1000))


def format_position(lat, lon):
    """Format lat/lon."""
    if lat is None or lon is None:
        return "N/A"
    ns = "N" if lat >= 0 else "S"
    ew = "E" if lon >= 0 else "W"
    return f"{abs(lat):.4f}°{ns} {abs(lon):.4f}°{ew}"

# ---------------------------------------------------------------------------
# Main spoke viewer
# ---------------------------------------------------------------------------

async def consume_for_duration(spoke_url, ws_ssl, duration):
    """Subscribe and discard frames for `duration` seconds (load driver
    for profiling — exercises the server's spoke send / compress path
    without doing any per-spoke work on the client side)."""
    print(f"  URL: {spoke_url}")
    print(f"  Reading for {duration}s, discarding spokes...")
    print()
    deadline = time.monotonic() + duration
    bytes_in = 0
    frames = 0
    # Disable client-side keepalive so a momentarily slow server (e.g.
    # under compression load) doesn't trigger a ping-timeout disconnect.
    async with websockets.connect(spoke_url, ssl=ws_ssl, ping_interval=None) as ws:
        while time.monotonic() < deadline:
            try:
                msg = await asyncio.wait_for(
                    ws.recv(), timeout=max(0.1, deadline - time.monotonic())
                )
            except asyncio.TimeoutError:
                break
            if isinstance(msg, (bytes, bytearray)):
                bytes_in += len(msg)
                frames += 1
    print(f"  Received {frames} frames, {bytes_in / 1024:.1f} KiB total"
          f" (avg {bytes_in / max(1, frames):.0f} bytes/frame)")


async def view_spokes(base_url, ws_url, insecure, duration=0):
    # Discover radar
    radar_id, radar_info = discover_radar(base_url)
    caps = fetch_capabilities(base_url, radar_id)

    spokes_per_rev = caps.get("spokesPerRevolution", 2048)
    max_spoke_len = caps.get("maxSpokeLength", 1024)
    sample_count = 32
    sample_interval = spokes_per_rev // sample_count

    # Print header
    print()
    print("=" * 78)
    print(f"  Radar:   {radar_info.get('name', radar_id)}")
    print(f"  Brand:   {radar_info.get('brand', '?')}")
    if radar_info.get("model"):
        print(f"  Model:   {radar_info['model']}")
    print(f"  ID:      {radar_id}")
    print(f"  Spokes/rev:  {spokes_per_rev}")
    print(f"  Max spoke:   {max_spoke_len} samples")
    print("=" * 78)
    print()
    print("Connecting to spoke stream...")

    spoke_url = radar_info["spokeDataUrl"]
    # Replace host if needed (server may advertise localhost)
    if ws_url:
        from urllib.parse import urlparse
        parsed_base = urlparse(ws_url)
        parsed_spoke = urlparse(spoke_url)
        spoke_url = spoke_url.replace(
            f"{parsed_spoke.scheme}://{parsed_spoke.netloc}",
            f"{parsed_base.scheme}://{parsed_base.netloc}",
        )

    print(f"  URL: {spoke_url}")
    print()

    ws_ssl = None
    if spoke_url.startswith("wss") and insecure:
        ssl_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ssl_ctx.check_hostname = False
        ssl_ctx.verify_mode = ssl.CERT_NONE
        ws_ssl = ssl_ctx

    if duration > 0:
        await consume_for_duration(spoke_url, ws_ssl, duration)
        return

    async with websockets.connect(spoke_url, ssl=ws_ssl) as ws:
        # Collect one revolution worth of spokes
        seen = set()
        sampled = []
        current_range = 0
        current_time = None
        current_lat = None
        current_lon = None
        rev_count = 0
        prev_angle = None

        while rev_count < 1:
            msg = await ws.recv()
            if isinstance(msg, str):
                continue

            radar_msg = pb2.RadarMessage()
            radar_msg.ParseFromString(msg)

            for spoke in radar_msg.spokes:
                # Detect revolution wrap
                if prev_angle is not None and spoke.angle < prev_angle:
                    rev_count += 1
                    if rev_count >= 1:
                        break
                prev_angle = spoke.angle

                current_range = spoke.range
                if spoke.HasField("time"):
                    current_time = spoke.time
                if spoke.HasField("lat"):
                    current_lat = spoke.lat
                if spoke.HasField("lon"):
                    current_lon = spoke.lon

                # Sample this spoke?
                bucket = spoke.angle // sample_interval
                if bucket not in seen:
                    seen.add(bucket)
                    sampled.append(spoke)

        # Sort by angle
        sampled.sort(key=lambda s: s.angle)

        # Print range info
        print(f"  Range:    {format_range(current_range)} ({current_range} m)")
        print(f"  Time:     {format_time(current_time)} UTC")
        print(f"  Position: {format_position(current_lat, current_lon)}")
        print()

        # Print spoke field documentation
        print("Spoke fields (from RadarMessage.proto):")
        print("  angle   - Rotation from bow [0..{}) clockwise".format(spokes_per_rev))
        print("  bearing - True bearing from North (optional)")
        print("  range   - Range of last pixel in meters")
        print("  time    - Epoch milliseconds (optional)")
        print("  lat/lon - Radar position (optional)")
        print("  data    - Pixel intensity bytes (0 = no return)")
        print()

        # Legend
        print(f"  ASCII legend:  {'  '.join(f'{c}={i}' for i, c in enumerate(SHADE))}")
        print(f"                 (pixel values are mapped to {len(SHADE)} levels)")
        print()

        # Column headers
        hdr_angle = "angle"
        hdr_bearing = " brng"
        hdr_range = " range"
        hdr_len = "len"
        hdr_data = "spoke data"
        print(f"  {hdr_angle:>7s} {hdr_bearing:>6s} {hdr_range:>7s} {hdr_len:>4s}  {hdr_data}")
        print(f"  {'─' * 7} {'─' * 6} {'─' * 7} {'─' * 4}  {'─' * 64}")

        for spoke in sampled:
            angle_str = format_angle(spoke.angle, spokes_per_rev)
            bearing = spoke.bearing if spoke.HasField("bearing") else None
            bearing_str = format_bearing(bearing, spokes_per_rev)
            range_str = format_range(spoke.range)
            data_len = len(spoke.data)
            ascii_art = spoke_to_ascii(spoke.data)

            print(f"  {angle_str} {bearing_str} {range_str:>7s} {data_len:>4d}  {ascii_art}")

        print()
        print(f"  Sampled {len(sampled)} of {spokes_per_rev} spokes (1 per {sample_interval})")
        print()


def main():
    parser = argparse.ArgumentParser(description="Spoke viewer for mayara-server")
    parser.add_argument("--url", default="http://localhost:6502", help="Server base URL")
    parser.add_argument("--insecure", "-k", action="store_true",
                        help="Allow insecure TLS connections (self-signed certificates)")
    parser.add_argument("--duration", type=float, default=0,
                        help="Run as a load driver: subscribe and discard frames for N seconds, "
                             "then exit. Skips per-revolution sampling and printing.")
    args = parser.parse_args()

    global verify_tls
    if args.insecure:
        verify_tls = False

    ws_url = args.url.replace("http://", "ws://").replace("https://", "wss://")

    try:
        asyncio.run(view_spokes(args.url, ws_url, args.insecure, args.duration))
    except KeyboardInterrupt:
        print("\nInterrupted.")
    except requests.ConnectionError:
        if args.url.startswith("https://"):
            sys.exit(f"Cannot connect to {args.url}. If using a self-signed certificate, pass --insecure.")
        else:
            sys.exit(f"Cannot connect to {args.url}. Is mayara-server running?")


if __name__ == "__main__":
    main()
