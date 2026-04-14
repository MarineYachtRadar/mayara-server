# How to deploy (install) mayara using Docker or Podman

Pre-built images for `amd64` and `arm64` are available on GitHub Container Registry:

    docker pull ghcr.io/marineyachtradar/mayara-server:latest

## Quick start (emulator)

    docker run -p 6502:6502 ghcr.io/marineyachtradar/mayara-server:latest mayara-server --emulator

## Real radar

Radar discovery uses multicast/broadcast, so the container needs direct network access:

    docker run --net=host \
        --read-only --security-opt no-new-privileges:true --cap-drop ALL \
        --tmpfs /home/mayara/.config/mayara \
        --tmpfs /home/mayara/.local/share/mayara \
        --tmpfs /tmp \
        ghcr.io/marineyachtradar/mayara-server:latest \
        mayara-server --brand navico --interface eth0

> **Note:** Real radar discovery relies on multicast/broadcast traffic. On Linux, use `--net=host` (or `network_mode: host` in Compose) to give the container direct network access. The emulator works fine with regular bridge networking.


## Docker compose

Or use Docker Compose (starts the emulator by default):

```bash
docker compose -f docker/docker-compose.yml up
```

See `docker/docker-compose.yml` for ready-made examples including emulator, real radar, TLS, and shore-based setups.

### Persistent data

To keep configuration and recordings across restarts, mount host directories owned by UID/GID `1000`:

    sudo mkdir -p /srv/mayaraserver-data/{config,recordings}
    sudo chown 1000:1000 /srv/mayaraserver-data/config /srv/mayaraserver-data/recordings

Then add volume mounts:

    -v /srv/mayaraserver-data/config:/home/mayara/.config/mayara:rw
    -v /srv/mayaraserver-data/recordings:/home/mayara/.local/share/mayara/recordings:rw
