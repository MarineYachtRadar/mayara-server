# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **furuno:** add anti-jamming filter control (0xE8) (#102) ([PR #102](https://github.com/MarineYachtRadar/mayara-server/pull/102))
- **replay:** pcap replay infrastructure for all radar brands (#99) ([PR #99](https://github.com/MarineYachtRadar/mayara-server/pull/99))
- **furuno:** 252-level palette, Tile echo decoder, and EchoFormat control (#108) ([PR #108](https://github.com/MarineYachtRadar/mayara-server/pull/108))
- **navico:** scanner type identification, capability bitmasks, and deferred radar visibility (#116) ([PR #116](https://github.com/MarineYachtRadar/mayara-server/pull/116))
- **navdata:** seed --static-position for real radars (#119) ([PR #119](https://github.com/MarineYachtRadar/mayara-server/pull/119))

### Changed

- **furuno:** add FAR radar setup guide (#85) ([PR #85](https://github.com/MarineYachtRadar/mayara-server/pull/85))
- **contributing:** add workflow guide, CodeRabbit config, scoped changelog (#95) ([PR #95](https://github.com/MarineYachtRadar/mayara-server/pull/95))
- add end-user guide, brand setup pages, and restructure documentation (#113) ([PR #113](https://github.com/MarineYachtRadar/mayara-server/pull/113))
- reduce pub visibility to pub(crate) and fix warnings (#123) ([PR #123](https://github.com/MarineYachtRadar/mayara-server/pull/123))

### Fixed

- prefer spoke heading over stale global heading in antenna offset (#82) ([PR #82](https://github.com/MarineYachtRadar/mayara-server/pull/82))
- **emulator:** propagate RangeUnits set_value errors (#83) ([PR #83](https://github.com/MarineYachtRadar/mayara-server/pull/83))
- **stream:** dedupe per-client radar metadata tracking (#87) ([PR #87](https://github.com/MarineYachtRadar/mayara-server/pull/87))
- **stream:** handle navigation and target paths in desubscribe (#88) ([PR #88](https://github.com/MarineYachtRadar/mayara-server/pull/88))
- **furuno:** restore DRS4W short ranges (1/8, 1/4, 1/2 NM) (#93) ([PR #93](https://github.com/MarineYachtRadar/mayara-server/pull/93))
- **cliff:** anchor CodeRabbit-fixup skip rules to commit subject (#96) ([PR #96](https://github.com/MarineYachtRadar/mayara-server/pull/96))
- **furuno:** handle 0x7D as radar alarm, not DRS4W heartbeat (#100) ([PR #100](https://github.com/MarineYachtRadar/mayara-server/pull/100))
- **furuno:** handle 0xAF as ARPA alarm, not heartbeat (#101) ([PR #101](https://github.com/MarineYachtRadar/mayara-server/pull/101))
- **furuno:** sqrt echo curve for DRS4W low-power radars (#111) ([PR #111](https://github.com/MarineYachtRadar/mayara-server/pull/111))
- **network:** remove overly strict broadcast address assertion (#118) ([PR #118](https://github.com/MarineYachtRadar/mayara-server/pull/118))
- **arpa:** stop flooding broadcast channel with per-blob target updates (#120) ([PR #120](https://github.com/MarineYachtRadar/mayara-server/pull/120))

## [3.4.2] - 2026-04-10

### Added

- **recording:** Re-implement radar recording and playback ([PR #33](https://github.com/MarineYachtRadar/mayara-server/pull/33))
- add Docker setup with CI and production hardening
- add optional TLS support
- **arpa:** require 4 updates for promotion, revolution-based deletion, dedup
- **arpa:** strong-return blobs, Doppler auto-track, ClearTargets command
- **arpa:** persist doppler_auto_track setting across restarts
- **furuno:** add DRS4W model detection
- **emulator:** loop simulation when targets leave radar range (closes #38)
- **web:** listen on IPv6 dual-stack socket for IPv4+IPv6 support
- **furuno:** add range units support for NM and km modes
- **furuno:** add dual range support for NXT models
- **furuno:** add per-model capabilities, tuning, and new command IDs
- **furuno:** add timed idle (watchman) controls for NXT radars
- **furuno:** add DRS4W full spoke support (#48)
- **navico:** add antenna offset controls (forward/starboard)
- **radar:** apply antenna offset to spoke positions
- **garmin:** comprehensive Garmin radar support
- **furuno:** display current pulse width from radar
- **furuno:** API support current pulse width from radar ([PR #71](https://github.com/MarineYachtRadar/mayara-server/pull/71))
- **furuno:** send guard zone geometry to radar hardware
- **furuno:** send guard zone geometry to radar hardware ([PR #70](https://github.com/MarineYachtRadar/mayara-server/pull/70))
- **furuno:** add STC curve controls for sea clutter suppression (#74) ([PR #74](https://github.com/MarineYachtRadar/mayara-server/pull/74))

### Changed

- **api:** unwrap /radars response, remove version wrapper
- **agents:** require CHANGELOG entry for each change
- **docker:** add deployment instructions for Docker
- **furuno:** clarify start_data_socket control flow
- **furuno:** simplify run() retry loop
- **target:** include result in blob processing log
- update USAGE.md for navigation address formats
- replace 2*PI with TAU
- add ARPA target tracking overview
- **furuno:** add wire protocol reference and dual range architecture
- **furuno:** add TCP command reference and convert capture files
- **furuno:** update research docs with live DRS4D-NXT findings
- **furuno:** add source attribution and dual range pcap capture
- **target:** use detector-wide spatial index for blob adjacency
- **changelog:** add Furuno DRS4D-NXT image quality fixes
- **navico:** replace fixed beacon structs with dynamic parser
- **navico:** rename state packet structs to match NRP protocol names
- **navico:** collect protocol constants in a dedicated module
- **furuno:** consolidate protocol constants into protocol.rs
- **furuno:** consolidate protocol constants into protocol.rs #68 ([PR #68](https://github.com/MarineYachtRadar/mayara-server/pull/68))
- **agents:** add CR review and test requirements before PR

### Fixed

- **openapi:** align schema names and readOnly fields with openApi.ts
- Show all IPv4 addresses per interface in Interfaces API
- **api:** correct HTTP status codes and improve error messages
- return 404 for NoSuchRadar and InvalidControlId errors
- TLS compatibility for WebSocket URLs and handlers
- make TLS certificate bypass opt-in in client examples
- **ws:** resume spokes stream on broadcast lag instead of disconnecting
- **navico:** accept 0xc2 as valid spoke status (#27)
- **docker:** add writable mount for recordings data directory
- **furuno:** retry spoke data sockets on creation failure ([PR #44](https://github.com/MarineYachtRadar/mayara-server/pull/44))
- **controls:** set timestamps on all control value changes
- **controls:** skip button controls in state broadcasts
- **controls:** prevent duplicate broadcasts for unchanged values
- **spoke:** decouple blob detection from spoke broadcasting
- **navico:** use correct nibble in doppler lookup table
- **spoke:** validate pixel values against full legend size
- **gui:** show disconnected state and fix standby overlay
- **replay:** allow target controls to be set in replay mode
- **gui:** correct target acquire endpoint URL
- **target:** increase max blob size to 1000m
- **controls:** handle all target controls in process_control_update
- **spoke:** broadcast heading from spoke data for GUI
- **navdata:** convert NMEA HDT heading to radians
- **navdata:** validate heading and COG range
- **gui:** log heading changes and loss
- **nmea:** convert VTG COG from degrees to radians
- **tracker:** use Kalman SOG for turn rejection speed gate
- **furuno:** accept non-NM range display units
- **gui:** handle do_change before control state is received from server
- **gui:** clear known targets when radar goes to disconnected state
- **spoke:** replace panic with error log in pixel validation
- **spoke:** replace panic with error log in pixel validation
- **navico:** accept HALO20+ spoke status 0xC2
- **gui:** avoid DISCONNECTED flash when spoke stream restarts in Firefox
- **network:** set IP_MULTICAST_IF for reliable multicast on multi-homed hosts
- **raymarine:** add main bang suppression control for HD models (fixes #35)
- **recordings:** add missing upload endpoint
- **recordings:** add missing upload endpoint ([PR #45](https://github.com/MarineYachtRadar/mayara-server/pull/45))
- **furuno:** correct spoke frame header field extraction
- **furuno:** correct dual range spoke routing and header parsing
- **furuno:** correct drid field positions for dual range commands
- **furuno:** pad short spokes and correct drid field positions
- **furuno:** stretch DRS4W spokes to FURUNO_SPOKE_LEN
- **range:** classify 125m as metric so it doesn't pollute the nautical list
- **furuno:** size FURUNO_SPOKE_LEN for DRS4D-NXT sweep_len=884
- **webui:** count unique angles in Reduce processor calibration
- **target:** circular arithmetic for blob spoke extent and center
- **web:** use numeric input for meter and degree controls
- **navico:** use i32 (not u16) for antenna height
- **navico:** align HALO heading/nav transmission with real MFDs
- **navico:** align HALO heading/nav/speed transmission with radar_pi
- **furuno:** DRS4W per-wire-index effective sample count
- **furuno:** use header scale field for spoke-to-range mapping
- **furuno:** apply 2x software gain for DRS4W/DRS echo intensity
- **furuno:** preserve control state when updating definitions at model detection
- **furuno:** handle auto-only control requests and address CR feedback
- **furuno:** restrict DRS4W range table to hardware-supported ranges
- **furuno:** restrict DRS4W range table to hardware-supported ranges #67 ([PR #67](https://github.com/MarineYachtRadar/mayara-server/pull/67))
- **furuno:** guard length check on $N70 guard status notification
- **ci:** use PR for changelog commit to respect branch protection (#73) ([PR #73](https://github.com/MarineYachtRadar/mayara-server/pull/73))
- **ci:** fall back to immediate merge when auto-merge not needed (#77) ([PR #77](https://github.com/MarineYachtRadar/mayara-server/pull/77))

[Unreleased]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.4.2...HEAD
[3.4.2]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.4.1...v3.4.2



## [3.4.1]

### Added

- Optional TLS support (`--tls-cert` and `--tls-key` flags) (#36, #37)
- Furuno DRS4W model detection and full spoke support (#48)
- ARPA target tracking documentation (`docs/arpa.md`)
- Furuno range units support: Nautical (NM) and Metric (km) modes with per-model range tables
- Furuno dual range support for NXT models: two independent radar instances (Range A/B) with shared TCP/UDP connections
- Furuno per-model capability system: controls are now enabled based on the detected radar model
- Furuno tuning control (auto/manual) for all models that support it
- New Furuno command IDs: PulseWidth, Tune, TrailMode, RingSuppression, Heartbeat, NN3Command
- Timed idle (watchman) controls for Furuno NXT radars: on/off toggle and transmit period
- Navico HALO antenna offset controls (forward/starboard) — read from StateProperties (0xC406) and settable via 0xC130 tag 4
- Navico spoke positions are now adjusted by the antenna offset (rotated by vessel heading) so ARPA targets appear at the antenna's actual ground position
- Navico protocol constants collected in a dedicated `protocol.rs` module with named opcode/command/multicast constants

### Changed

- Navico beacon parsing uses dynamic device/service format for all models (BR24, 3G, 4G, HALO) — replaces the previous fixed-size struct discrimination by length
- Navico state packet structs renamed to match the NRP protocol names (StateMode, StateSetup, StateConfig, StateFeatures, StateProperties, StateInstallation)
- Navico antenna height now uses i32 (was u16) — matches the wire protocol and supports heights >65 m
- Navico HALO heading/navigation/speed transmitter rewritten to match the radar_pi reference implementation: correct heading scale (0..0xF800 = 0..360°), correct SOG units (cm/s for navigation, dm/s for speed), separate timers per packet type (heading 100 ms, navigation 250 ms, speed 250 ms), 10-second listen-and-defer timeout
- GUI uses numeric input instead of sliders for meter and degree valued controls
- Emulator loops continuously: boat and targets reverse course when targets leave radar range, then turn back at the starting position (closes #38)
- Web server listens on IPv6 dual-stack socket, accepting both IPv4 and IPv6 connections
- WebSocket URLs use `wss://` scheme when TLS is enabled
- Client examples accept `--insecure`/`-k` flag for self-signed certificates (opt-in, no longer default)

### Fixed

- Target tracker pegged one CPU core on noisy feeds: the blob detector now uses a single detector-wide `(spoke, pixel) -> blob id` spatial index, so adjacency and contour lookups are O(1) in both blob size and number of active blobs
- Target tracker reported wrong size and center for blobs spanning spoke 0: `BlobInProgress` tracked spoke extents with linear min/max, so a wrap-around blob would report a center on the opposite side of the revolution and a size covering nearly the whole circle. Replaced with on-demand smallest-covering-arc computation that handles the circular spoke domain correctly (#58)
- Furuno DRS4D-NXT: raised FURUNO_SPOKE_LEN from 883 to 1024 — the DRS4D-NXT reports sweep_len=884, so each spoke's last sample overflowed into the next angle's slot and produced a slowly rotating ring/moiré pattern on the PPI
- Furuno DRS4D-NXT: Reduce processor calibration now counts unique angles — the radar sends each angle twice in consecutive sweeps, so the old count was roughly 2× the true unique-angle count and left every other reduced-buffer slot empty, producing tangential striping across rendered targets
- Furuno range zoom was stuck at 1/16 NM (116m) after the closest-match `lookup_wire_index` change: the 125m km-table entry was misclassified as nautical by the metric heuristic and polluted the nautical range list, so zooming out from 116m sent 125m which then mapped back to wire index 21 (116m) — a no-op. 125m is now special-cased as metric
- Furuno dual range: Route TCP report responses (Status, Gain, Sea, Rain, Tune) to the correct range (A or B) based on dual_range_id in the response
- Furuno dual range: correct drid field positions for all per-range commands (Status, Gain, Sea, Rain) — verified against live Wireshark captures
- Furuno dual range: Range response now correctly reads unit from field 1 (was field 2, which is actually drid)
- Furuno spoke header: dual_range_id is at byte 15 bit 6 (was incorrectly at byte 11 bits 6-7, which are always 0b11)
- Furuno dual range: Range B spoke interleaving is no longer auto-activated at init; it starts after the first explicit Range B range command sent by the client
- Furuno tune control max increased from 100 to 2000 to accommodate raw radar values
- Furuno DRS4W: pad short spokes to sweep_len — compressed data on compact WiFi radars can produce fewer samples than expected (#48)
- Furuno spoke distance rendering: use the `scale` field from the UDP frame header (bytes 14-15) as the effective sample count instead of `sweep_len`. The radar always transmits `sweep_len` total samples but only the first `scale` of them cover the configured display range — using `sweep_len` caused targets to render at `scale/sweep_len` (~56%) of their true radial distance on all Furuno models (#48)
- Furuno DRS4W/DRS echo intensity: apply 2× software gain to compensate for the lower raw echo values produced by low-power magnetron antennas (max ~124 vs NXT's ~252), so the full color palette (blue→green→yellow→red) is utilised instead of only the lower half (#48)
- Furuno Gain/Sea/Rain control definitions now preserve runtime state (value, auto) when updated at model-detection time
- Furuno dual range: Range B ModelName is now vendor-accurate (the " B" suffix is only in UserName)
- Furuno spoke header: heading_valid now correctly read from byte 11 bit 5 (was reading byte 15 bits 4-5)
- Furuno spoke header: range wire index masked to 6 bits, angle/heading masked to 13 bits
- Furuno frequent heartbeat ($NAF) and NN3 diagnostic ($NF5) messages no longer cause log noise
- Raymarine HD main bang suppression control was missing, causing error in logs (#35)
- Multicast reception on multi-homed interfaces (multiple IPs on one NIC) (#51)
- Furuno range report no longer rejects radars set to km or sm display units
- All control values now include timestamps in state broadcasts
- Button controls (clearTrails, clearTargets) no longer sent in state broadcasts
- Unchanged control values no longer re-broadcast to clients
- Blob detection no longer blocks spoke broadcasting to clients
- Navico doppler lookup used wrong nibble for HighBoth mode
- Spoke pixel validation checks full legend size, not just normal colors
- Spoke pixel validation no longer panics in debug builds, logs error and clamps instead
- GUI shows "DISCONNECTED" when server connection is lost
- GUI standby overlay always shows ON-TIME/TX-TIME
- GUI WebSocket reconnect no longer creates duplicate connections
- Target controls (guard zones, exclusion zones) are writable in replay mode
- GUI target acquire used wrong REST endpoint URL
- Guard zones, exclusion rects, and other target controls returned 400 despite succeeding
- NMEA HDT heading was sent in degrees instead of radians
- NMEA VTG COG was sent in degrees instead of radians
- Slow/stationary targets repeatedly lost and reacquired due to turn rejection using noisy measured speed instead of Kalman-estimated speed
- Targets now require 4 updates before being promoted to tracking and displayed, reducing noise from clutter blobs
- Large vessels no longer produce multiple duplicate tracks; young targets within 100m are merged at each revolution end
- Lost targets deleted after 4 revolutions (was 30s); stationary targets after 10 revolutions
- Blob detection now requires strong return (not medium) and at least 25 pixels to suppress wave/clutter arcs
- Doppler-approaching targets tracked everywhere when doppler_auto_track is enabled, not only inside guard zones
- clearTargets button now actually clears all targets immediately from both backend and GUI
- doppler_auto_track setting is now persisted across restarts
- GUI do_change no longer crashes with TypeError when a control is changed before its state is received from the server
- GUI clears all displayed targets when radar connection is lost
- GUI showed DISCONNECTED in Firefox after server restart due to unnecessary state reset when spoke stream reconnect triggered a redundant state stream reconnect
- Heading extracted from spoke data for GUI when no external heading source available
- Furuno spoke data sockets retry on failure instead of silently staying dead
- Recording file upload from the GUI (add missing `POST /recordings/files/upload` route)
- Accept 0xc2 as valid Navico spoke status for HALO20+ compatibility (#27)
- Move inline `display: none` style to CSS for WebGPU warning element
- `NoSuchRadar` returns 404 (was 500), response includes list of valid radar IDs
- `InvalidControlId` returns 404 (was 400/500)
- Unmatched `/signalk/` paths return 404 with list of all valid API endpoints (generated from OpenAPI spec)
- Empty spoke messages no longer broadcast to WebSocket clients
- Spoke WebSocket stream no longer disconnects on broadcast lag (#31)

### Removed

- Duplicate protobuf.js library copies in `web/imports/` and `web/protobuf/` (only `web/gui/protobuf/` is used)

## [3.4.0]

### Changed

- **API version bumped to 3.2.0** (Signal K Radar API)
- `GET /signalk/v2/api/vessels/self/radars` returns bare radar map (removed `version`/`radars` wrapper)
- All REST endpoints now return unwrapped responses — no wrapper on any endpoint
- OpenAPI schema: `allowed`, `error`, and `timestamp` fields on ControlValue marked `readOnly`
- OpenAPI schema: renamed `RadarApiV3` to `RadarInfo`, `ArpaTargetApi` to `ArpaTarget`

### Removed

- `RadarsResponse`, `FullSignalKResponse`, and `wrap_response()` — no longer needed

## [3.3.0]

### Added

- `timestamp` field on control values, set whenever a value changes
- `hasSparseSpokes` capability field
- `GET /quit` endpoint for clean server shutdown
- `GET /signalk` endpoint for Signal K service discovery
- `SIGNALK_RADAR_API_VERSION` constant in `Cargo.toml [package.metadata]`, used at compile time
- Docker support: Dockerfile, Dockerfile.ci, docker-compose.yml, GitHub Actions workflow for multi-arch builds (#28)
- Integration test suite: 18 REST API tests and 10 WebSocket stream tests
- `tests/run-integration.sh` to start emulator, run all tests, and stop server
- `make test` now runs full integration tests against the emulator
- Client examples: Python, JavaScript, and Bash (`client-examples/`)
- `CLIENT.md` documenting the client examples
- Server logs PID on startup for test harness
- OpenAPI spec version patched at runtime from `SIGNALK_RADAR_API_VERSION`
- Network IPv4 requirements documented in USAGE.md

### Changed

- **API version bumped to 3.1.0** (Signal K Radar API)
- `GET /` redirects to `/gui/` instead of content-negotiating HTML vs JSON
- `GET /signalk` serves the endpoints JSON (moved from `/`)
- Unwrapped REST responses for consistency:
  - `GET .../interfaces` returns bare `InterfaceApi` instead of `{ version, radars: { interfaces: ... } }`
  - `GET .../{id}/capabilities` returns bare `Capabilities` instead of `{ version, radars: { id: { capabilities: ... } } }`
  - `GET .../{id}/controls` returns bare controls map instead of `{ version, radars: { id: { controls: ... } } }`
  - `GET .../{id}/controls/{control_id}` returns bare `ControlValue` (unchanged, was already unwrapped)
  - `GET .../{id}/targets` returns bare array instead of `{ timestamp, targets: [...] }`
- Only `GET /signalk/v2/api/vessels/self/radars` retains the `{ version, radars }` wrapper
- Renamed `lastChanged` to `timestamp` on control values
- GUI `api.js` and `mayara.js` updated for unwrapped responses
- Updated link to releases page in USAGE.md (#25)

### Removed

- Dead code cleanup

### Fixed

- Emulator `/interfaces` endpoint returns empty JSON instead of error text

## [3.2.0]

### Fixed

- Fix recursive call of error handler (#23)

### Added

- Garmin support for old Garmin radars (HD, xHD) (#21)

## [3.1.0]

First semver version. From here on all additions and fixes will be
logged as GitHub issues.

### Added

- Target tracking

## Versions

[3.4.1]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.4.0...v3.4.1
[3.4.0]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.3.0...v3.4.0
[3.3.0]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.2.0...v3.3.0
[3.2.0]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.1.0...v3.2.0
[3.1.0]: https://github.com/MarineYachtRadar/mayara-server/compare/v3.0.0...v3.1.0
