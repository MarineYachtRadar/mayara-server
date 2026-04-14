# Mayara for end users

Mayara gives you a radar display in any web browser. It works with most marine yacht radars from Navico, Raymarine, Furuno, and Garmin — see [README.md](README.md) for the full list of supported models.

## What you get

- A classic PPI (Plan Position Indicator) radar display in your browser
- Full radar control: power on/off, range, gain, sea clutter, rain clutter, interference rejection, and more
- ARPA target tracking with automatic CPA/TCPA calculation
- AIS overlay when connected to an AIS receiver
- Dual-range display on radars that support it
- Doppler/MotionScope on supported radars

No MFD (multi-function display) is required — Mayara talks directly to the radar over your boat's Ethernet network. It also works alongside your existing MFD without interfering.

## Requirements

- A computer running Windows (x86_64), macOS, or Linux (x86_64 or ARM64)
- A wired Ethernet connection to the radar
- A web browser on any device that can reach the computer running Mayara

Mayara is lightweight: 32 MB of RAM and minimal disk space is enough. It runs well on a Raspberry Pi or similar small computer.

A common setup is to run Mayara on a small wired device (like a Raspberry Pi) connected to the radar's Ethernet network, and then access the radar display from a phone, tablet, or laptop over WiFi. Radar data uses multicast which does not work well over WiFi directly, but Mayara bridges this gap by serving the processed data over standard HTTP/WebSocket which works fine over any network.

## Download

Pre-built binaries are available on the [GitHub releases page](https://github.com/mayara-server/mayara-server/releases).

Choose the right file for your platform:

| Platform                      | File                                                     |
| ----------------------------- | -------------------------------------------------------- |
| Linux x86_64                  | `mayara-server-vX.X.X-x86_64-unknown-linux-musl.tar.gz`  |
| Linux ARM64 (Raspberry Pi)    | `mayara-server-vX.X.X-aarch64-unknown-linux-musl.tar.gz` |
| macOS (Apple Silicon & Intel) | `mayara-server-vX.X.X-universal-apple-darwin.tar.gz`     |
| Windows                       | `mayara-server-vX.X.X-x86_64-pc-windows.zip`             |

## Installation

### Linux / macOS

Download and extract the archive:

```sh
tar xzf mayara-server-vX.X.X-*.tar.gz
```

Optionally, move the binary to a directory in your PATH:

```sh
sudo mv mayara-server /usr/local/bin/
```

### Windows

Extract the zip file to a folder of your choice. No installer is needed.

## Running Mayara

### Quick start

Connect your computer to the same Ethernet network as your radar, then start Mayara:

```sh
mayara-server
```

Mayara will automatically detect any radar on the network. Open a web browser and go to:

```
http://localhost:6502
```

You should see a list of detected radars. If you are accessing from another device on the same network, replace `localhost` with the IP address of the computer running Mayara.

Click on a radar to open the PPI (radar display). If the radar is in standby, click the power button in the top left to start transmitting. Use the hamburger menu (top right) to adjust gain, sea clutter, range, and other settings.

### Try it without a radar

If you don't have a radar connected, you can try the built-in emulator:

```sh
mayara-server --emulator
```

This simulates a radar with moving targets so you can explore the interface.

### Common options

| Option       | Description                                     |
| ------------ | ----------------------------------------------- |
| `--emulator` | Run with a simulated radar (no hardware needed) |
| `-p 8080`    | Use a different port (default: 6502)            |
| `-b navico`  | Only look for a specific brand                  |
| `-v`         | Verbose logging (repeat for more detail: `-vv`) |

For the full list of options, run `mayara-server --help` or see [USAGE.md](USAGE.md).

### Docker

For more technically oriented users, Mayara is also available as a Docker image:

```sh
docker run -p 6502:6502 ghcr.io/marineyachtradar/mayara-server:latest mayara-server --emulator
```

For real radar use, you need `--net=host` so Mayara can see the multicast traffic from the radar. See [docs/docker.md](docs/docker.md) for details.

## Troubleshooting

**No radar detected:**
- Make sure the computer is connected via wired Ethernet to the same network as the radar.
- Click the _Network_ button on the main page to check whether your Ethernet card has an IPv4 address in the right range for your radar.
- Check that the radar is powered on.
- Try specifying the network interface: `mayara-server -i eth0`
- WiFi connections to the radar are generally not supported (except Furuno DRS4W and Raymarine Quantum). Mayara skips WiFi interfaces by default — use `--allow-wifi` if your radar connects via WiFi.

**Brand-specific network requirements:**
- **Garmin and Furuno** radars require the network card to have an IPv4 address in a specific range. The _Network_ page will show if this is missing.
- **Raymarine** radars require a DHCP server on the network. This can be a Raymarine chartplotter, a network router, or a DHCP service on the computer.

**Browser can't connect:**
- Verify Mayara is running and note the port in the console output.
- If accessing from another device, use the computer's IP address instead of `localhost`.
- Check that no firewall is blocking port 6502.

## Need help?

Join us on Discord: [discord.gg/kC6h6JVxxC](https://discord.gg/kC6h6JVxxC)
