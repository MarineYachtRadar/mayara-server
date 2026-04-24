#!/usr/bin/env python3
"""
Target viewer for mayara-server.

Connects to a running mayara-server via WebSocket, subscribes to
ARPA/MARPA targets only, and prints the raw JSON updates to stdout.

Usage:
    python3 target_viewer.py [--url http://localhost:6502] [--insecure]

Requirements:
    pip install websockets requests
"""

import argparse
import asyncio
import json
import ssl
import sys

import requests
import websockets

# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

verify_tls = True


def discover_radars(base_url):
    """Fetch all radars from the API."""
    r = requests.get(f"{base_url}/signalk/v2/api/vessels/self/radars", verify=verify_tls)
    r.raise_for_status()
    return r.json()


# ---------------------------------------------------------------------------
# Main target viewer
# ---------------------------------------------------------------------------

async def view_targets(base_url, insecure):
    radars = discover_radars(base_url)
    if not radars:
        sys.exit("No radars found. Is the server running with --emulator or a real radar?")

    ws_scheme = "wss" if base_url.startswith("https") else "ws"
    host = base_url.split("://", 1)[1]
    ws_url = f"{ws_scheme}://{host}/signalk/v1/stream?subscribe=none"

    ws_ssl = None
    if ws_scheme == "wss" and insecure:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        ws_ssl = ctx

    print(f"Connecting to {ws_url} ...")

    async with websockets.connect(ws_url, ssl=ws_ssl, compression=None) as ws:
        # Subscribe to targets only, with instant updates
        subscribe_msg = json.dumps({
            "subscribe": [
                {"path": "radars.*.targets.*", "policy": "instant"}
            ]
        })
        await ws.send(subscribe_msg)
        print(f"Subscribed to targets. Waiting for updates...\n")

        async for raw in ws:
            msg = json.loads(raw)
            print(json.dumps(msg, indent=2))


def main():
    parser = argparse.ArgumentParser(description="ARPA/MARPA target viewer for mayara-server")
    parser.add_argument("--url", default="http://localhost:6502", help="Server base URL")
    parser.add_argument("--insecure", "-k", action="store_true",
                        help="Allow insecure TLS connections (self-signed certificates)")
    args = parser.parse_args()

    global verify_tls
    if args.insecure:
        verify_tls = False

    try:
        asyncio.run(view_targets(args.url, args.insecure))
    except KeyboardInterrupt:
        print("\nBye.")
    except requests.ConnectionError:
        if args.url.startswith("https://"):
            sys.exit(f"Cannot connect to {args.url}. If using a self-signed certificate, pass --insecure.")
        else:
            sys.exit(f"Cannot connect to {args.url}. Is mayara-server running?")


if __name__ == "__main__":
    main()
