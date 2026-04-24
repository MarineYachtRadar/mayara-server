# Adding a New Radar Brand

This guide covers every step needed to add support for a new marine radar brand to mayara-server.

## Prerequisites

Before writing code, you need:

- **Network captures** of the radar's traffic (pcap format). Use Wireshark or tcpdump to capture all multicast/broadcast UDP traffic while the radar boots, transmits, and responds to control changes.
- **Protocol documentation** if available, or reverse-engineered knowledge of the wire format: discovery packets, spoke data encoding, command formats, and status reports.
- A **radar-recordings** repository checkout (sibling to this repo) if you want to generate test fixtures.

## Module Structure

Each brand lives in `src/lib/brand/{brand_name}/` with these files:

```
src/lib/brand/{brand_name}/
  mod.rs          Locator: network discovery, beacon parsing
  protocol.rs     Wire-level constants (addresses, ports, opcodes, packet layouts)
  report.rs       Report receiver: parses incoming spoke data
  command.rs      Command sender: formats and sends control commands
  settings.rs     Control definitions (gain, rain, range, etc.)
```

Some brands add optional files for complex features:

- `capabilities.rs` — runtime feature/capability detection
- `discovery.rs` — separate discovery logic (Garmin CDM heartbeat)
- `info.rs` — model-specific metadata (Navico model table)
- `range_table.rs` — supported range values (Garmin)

Use Koden (`src/lib/brand/koden/`) as a minimal reference, or Navico for a full-featured example.

## Step-by-Step Checklist

### 1. Add the Brand enum variant

**File:** `src/lib/mod.rs`

Add your brand to the `Brand` enum and implement `to_prefix()` with a unique 3-letter code used for radar key generation (e.g. `"nav"` produces keys like `nav1034A`):

```rust
pub enum Brand {
    // ... existing brands ...
    YourBrand,
}

impl Brand {
    pub fn to_prefix(&self) -> &'static str {
        match self {
            // ...
            Self::YourBrand => "yor",
        }
    }
}
```

Also update the `From<String>`, `Serialize`, and `Display` implementations in the same file.

### 2. Add the Cargo feature flag

**File:** `Cargo.toml`

```toml
[features]
yourbrand = []
# Add to default if the brand should be compiled by default:
default = ["navico", "furuno", "garmin", "koden", "raymarine", "yourbrand", "emulator"]
```

### 3. Create the module and register it

**File:** `src/lib/brand/mod.rs`

Add the conditional module and a `LocatorId` variant:

```rust
#[cfg(feature = "yourbrand")]
pub(crate) mod yourbrand;

pub(crate) enum LocatorId {
    // ... existing ...
    YourBrand,
}
```

Register the listener in `create_brand_listeners()`:

```rust
#[cfg(feature = "yourbrand")]
if args.brand.unwrap_or(Brand::YourBrand) == Brand::YourBrand {
    yourbrand::new(args, listen_addresses);
    brands.insert(Brand::YourBrand);
}
```

### 4. Implement `protocol.rs`

Define the wire-level constants that describe how your radar communicates:

```rust
use std::net::{Ipv4Addr, SocketAddrV4};

// Network addresses — these come from your pcap analysis
pub const BEACON_ADDRESS: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(239, 0, 0, 1), 5678);
pub const SPOKE_DATA_ADDRESS: SocketAddrV4 = ...;
pub const REPORT_ADDRESS: SocketAddrV4 = ...;
pub const COMMAND_ADDRESS: SocketAddrV4 = ...;

// Optional: packet to send to trigger discovery response
pub const DISCOVERY_QUERY: &[u8] = &[0x01, 0x02, ...];

// Radar geometry
pub const SPOKES: u16 = 2048;          // spokes per revolution
pub const SPOKE_LEN: u16 = 1024;       // max pixels per spoke
pub const PIXEL_VALUES: u8 = 16;       // color depth (typically 4 or 16)
```

### 5. Implement `mod.rs` (Locator)

The locator listens for discovery packets and creates radars when detected.

Implement `RadarLocator` trait:

```rust
pub(crate) trait RadarLocator: Send {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error>;

    fn clone(&self) -> Box<dyn RadarLocator>;
}
```

And provide the `new()` function that registers your locator address:

```rust
pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::YourBrand) {
        addresses.push(LocatorAddress::new(
            LocatorId::YourBrand,
            &BEACON_ADDRESS,
            Brand::YourBrand,
            vec![],  // or vec![&DISCOVERY_QUERY] if active discovery is needed
            Box::new(YourBrandLocator::new(args.clone())),
        ));
    }
}
```

When a beacon is recognized, create a `RadarInfo` and start the report receiver:

```rust
let info = RadarInfo::new(
    radars, &self.args, Brand::YourBrand,
    Some("serial"),       // serial number from beacon, or None
    None,                 // dual radar suffix (Some("A") for dual-range)
    PIXEL_VALUES,
    SPOKES,
    SPOKE_LEN,
    radar_addr,           // radar's IP:port
    *nic_addr,            // network interface that received the beacon
    SPOKE_DATA_ADDRESS,
    REPORT_ADDRESS,
    COMMAND_ADDRESS,
    |id, tx| settings::new(id, tx, &self.args),
    false,                // supports doppler?
    false,                // sparse spokes?
);

// Start report receiver
subsys.start(SubsystemBuilder::new("yourbrand-report", |s| {
    report::YourBrandReportReceiver::new(info).run(s)
}));
```

### 6. Implement `report.rs`

The report receiver listens for spoke data and broadcasts it to WebSocket clients:

```rust
pub(crate) struct YourBrandReportReceiver {
    info: Arc<RadarInfo>,
}

impl YourBrandReportReceiver {
    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        // Listen on the spoke data address
        let mut sock = create_listening_socket(&self.info.spoke_data_addr)?;

        loop {
            let (len, _from) = sock.recv_buf_from(&mut buffer).await?;

            // Parse brand-specific packet format
            // Extract: angle, bearing, range, pixel data
            // Build RadarMessage protobuf and broadcast:
            self.info.broadcast_radar_message(message);
        }
    }
}
```

The protobuf message format (`RadarMessage.proto`) is shared across all brands:

```protobuf
message RadarMessage {
    message Spoke {
        uint32 angle = 1;            // [0..spokes_per_revolution)
        optional uint32 bearing = 2; // true bearing from North
        uint32 range = 3;            // range of last pixel in meters
        optional uint64 time = 4;    // epoch milliseconds
        optional double lat = 6;     // radar latitude
        optional double lon = 7;     // radar longitude
        bytes data = 5;              // pixel intensity bytes
    }
    repeated Spoke spokes = 2;
}
```

### 7. Implement `command.rs`

The command sender translates GUI control changes into wire-format commands:

```rust
#[async_trait]
impl CommandSender for YourBrandCommand {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        match cv.id {
            ControlId::Power => { /* format and send power command */ }
            ControlId::Gain => { /* format and send gain command */ }
            ControlId::Range => { /* format and send range command */ }
            // ...
        }
    }
}
```

### 8. Implement `settings.rs`

Define which controls are available and how they map to wire values:

```rust
pub fn new(
    radar_id: String,
    sk_client_tx: broadcast::Sender<SignalKDelta>,
    args: &Cli,
) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::ModelName).read_only(true).build(&mut controls);
    new_numeric(ControlId::Gain, 0., 100.)
        .wire_scale_factor(2.55, false)  // map 0-100 to 0-255 on wire
        .build(&mut controls);
    new_list(ControlId::InterferenceRejection, &["Off", "Low", "Medium", "High"])
        .build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}
```

Control types: `new_numeric`, `new_list`, `new_sector`, `new_auto`, `new_string`. See `src/lib/radar/settings.rs` for the full builder API.

## Testing

### Generate a pcap fixture

1. Place your full capture in the `radar-recordings` repo (sibling directory)
2. Add a `generate_fixture()` call to `examples/generate-fixtures.rs` with a filter for your brand's multicast addresses/ports:

```rust
generate_fixture(
    &base.join("yourbrand/capture.pcap.gz"),
    &fixture_dir.join("yourbrand-model.pcap.gz"),
    &|p| {
        let port = p.dst_addr.port();
        port == 5678 || port == 5679
    },
    500,  // max packets to keep
);
```

3. Run the generator:

```sh
cargo run --features pcap-replay --example generate-fixtures
```

This creates a small filtered fixture in `testdata/pcap/yourbrand-model.pcap.gz`.

### Write a replay integration test

Create `tests/replay_yourbrand.rs`:

```rust
use mayara::{replay, Cli, Brand};
use std::path::Path;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};

#[tokio::test]
async fn replay_yourbrand() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/pcap/yourbrand-model.pcap.gz");
    if !fixture.exists() {
        panic!("Fixture not found: {}", fixture.display());
    }

    replay::init(&fixture).expect("init replay");
    replay::set_instant_timing();

    let args = Cli {
        brand: Some(Brand::YourBrand),
        pcap: Some("fixture".to_string()),
        // ... other fields with defaults (see existing replay tests)
        ..Default::default()
    };

    Toplevel::new(move |s| async move {
        let (radars, _) = mayara::start_session(&s, args).await;
        s.start(SubsystemBuilder::new("test", move |subsys| async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let keys = radars.get_keys();
                if !keys.is_empty() {
                    assert!(keys[0].starts_with("yor"), "expected YourBrand key");
                    let info = radars.get_by_key(&keys[0]).unwrap();
                    assert_eq!(info.brand, Brand::YourBrand);
                    break;
                }
                if tokio::time::Instant::now() > deadline {
                    panic!("Timeout: no radar detected");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            subsys.request_shutdown();
            Ok::<(), miette::Report>(())
        }));
    })
    .handle_shutdown_requests(Duration::from_millis(2000))
    .await
    .expect("toplevel");
}
```

Run it:

```sh
cargo test --features pcap-replay,yourbrand --test replay_yourbrand
```

### Run the full test suite

```sh
cargo test --features yourbrand
```

## Network Discovery Patterns

Different brands use different discovery mechanisms. Common patterns:

| Pattern | Example | How it works |
|---------|---------|-------------|
| Multicast beacon | Navico, Raymarine | Radar broadcasts periodic beacons to a well-known multicast group |
| Active query | Navico Gen3+ | Server sends a query packet; radar responds with its addresses |
| Broadcast heartbeat | Furuno, Koden | Radar sends UDP broadcasts on the local subnet |
| CDM heartbeat | Garmin | Separate heartbeat protocol provides radar addresses |

Your `LocatorAddress` registration specifies:
- The multicast/broadcast address to listen on
- Optional query packets to send (for active discovery)
- The `RadarLocator` that parses incoming packets

## Reference: Existing Brands

| Brand | Prefix | Locators | Discovery | Models |
|-------|--------|----------|-----------|--------|
| Navico | `nav` | GenBR24, Gen3Plus | Multicast query+response | BR24, 3G, 4G, HALO |
| Furuno | `fur` | Furuno | Subnet broadcast | DRS4D-NXT |
| Garmin | `gar` | Garmin, GarminCdm | CDM heartbeat | HD, xHD |
| Koden | `kod` | Koden | Subnet broadcast | RADARpc |
| Raymarine | `ray` | Raymarine | Multicast beacon | RD, Quantum |
