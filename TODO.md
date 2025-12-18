# TODO.md

### Kees
Working:

* Start, logging
* Detect BR24, 3G, 4G and HALO radars
* Detect Raymarine radars (tested with Quantum 2)
* Detect Furuno radars (tested with DRS4D-NXT)
* Provide webserver for static and dynamic pages
* Serve Navico and Furuno radar data
* Control Navico radar (tested with 4G and HALO)
* Trails in relative mode
* Getting heading and location from Signal K or NMEA 0183 server

Work in Progress:

* Target acquisition (M)ARPA
* Detect Garmin xHD (but not yet know if different from HD)
* Furuno control - implemented for DRS4D-NXT (except ARPA)

TODO:

* Guard zones
* Everything else


### dirkwa

- Test HALO radar @Kees/PI4
  - Get GUI working, find all race conditions etc. 
  - Update protocol
  - Verify and extend radar model database in core.
  - Verify and extend tests
  - Update documentation
  - Update README
  - Prepare PR
- Make new SignalK connector for mayara-server standalone as provider and client (similar to WASM).
  - doc folder in server or plugin?
  - 
- For SignalK devs - Plugin too use history and playback Radar Spokes


#### diwa done
- Find a github org home for the project and rename evreything
- Update GITHUB build infrastructure and NPM publish for new infrastructure
- Full refactor
- Refactor WASM to new infrastructure
- Update documentation to WASM

### unassigned
- OpenCPN plugin
- Test more radars for new architecture.


### delme
./target/debug/mayara-server -p 6502 -v 2>&1 | grep -E "report 01|status|power"

