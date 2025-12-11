### MAYARA

Welcome to **Ma**rine **Ya**cht **Ra**dar server.

This project will play as intermediary between marine yacht radars such as Navico's HALO series, Furuno, Garmin, Raymarine, etc, and modern client side tools acting as chartplotter or MFD. 
Intended use is for applications such as [Freeboard SK](https://github.com/SignalK/freeboard-sk), [OpenCPN](https://opencpn.org), [AvNav](https://wellenvogel.net/software/avnav/docs/beschreibung.html?lang=en).
__Note: no implication that this software will actually be available in any of the mentioned software packages is made!__

On the "client" side, it will offer a [Signal K](https://signalk.org) API for basic information and a `WebSocket` server for the actual radar data.
Changing the radar settings is possible, a [JSON Schema](https://json-schema.org) explains what settings can be made.

## Origins

This is basically a rewrite of the [OpenCPN radar plugin](https://github.com/opencpn-radar-pi/radar_pi) that I have worked over ten years or so.
The problem with that code is that it is written in C++ with wxWidgets, and very much meant to operate as a plugin to OpenCPN. That makes it hard to graft on
an extra layer that allows it to be used in other contexts.

## Philosophy

The code shall:

* Be able to run independently, and provide a simple API for clients to use. This shall be 'friendly' to web based software.
* As far as possible, detect all radars automatically without any configuration; in the `radar_pi` plugin you had to set which brand/type of radar is installed.
* Make it as simple as possible to add more radars. Our experience with `radar_pi` tells us that there are hardly any folks out there cruising with the right skillset to make this happen.
* Be robust and error-free. Again, C++ allows you to be doing stuff illegally and for many years we had race conditions and other bugs in `radar_pi`. Writing the new server in Rust will hopefully make this an easy thing to do.

## Radar support

The following radars are fully supported right now:

* Navico: all digital models e.g. BR24, 3G, 4G, HALO20, HALO24, HALO3/4/6, HALO3000+. 

Full support is planned for:

* Furuno, many models, but certainly and primarily DRS4D-NXT.
* Raymarine Quantum 2 and older digital radars.
* Garmin (x)HD.

## Important Notes

### Furuno Multi-Client Behavior

Furuno radars support multiple TCP clients (multi-master mode), but model detection requires receiving the initial connect reply (`$N96` message). If another client (e.g., NavNet TZtouch, TimeZero) is already connected when mayara starts, this message won't be sent again, and mayara won't be able to detect the radar model or populate the range list.

**Workaround**: Ensure mayara connects to the radar before other clients, or temporarily disconnect other clients and restart mayara.

## Status

See [TODO](TODO.md)

## Help us

We're on Discord, here is an invite: [Discord channel](https://discord.gg/kC6h6JVxxC)


