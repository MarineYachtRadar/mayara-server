# Showing Any Radar on a Garmin Chartplotter

A Garmin GPSMAP chartplotter only draws radar from a Garmin radar. Mayara can
close that gap: built with the `garmin-xhd-output` feature it emulates a Garmin
GMR xHD on the Garmin Marine Network, and feeds it the picture of whichever
radar it found — a Furuno DRS, a Navico HALO, a Raymarine Quantum, a Koden, or
the built-in emulator. The plotter discovers it by itself and shows it in the
radar list as a GMR xHD.

The controls work in both directions. Range, gain, sea clutter, rain clutter,
and the transmit button on the plotter change the setting on the real radar,
and a change made from Mayara's own GUI or another display shows up on the
plotter.

> **Not yet validated on a chartplotter.** The bridge has been verified against
> Mayara's own Garmin receiver over loopback — discovery, the report stream,
> controls and spoke data all check out — but nobody has yet put it in front of
> a physical Garmin GPSMAP. If you try it, please report how it goes.

## What you need

- Mayara built with the feature (it is not in the standard binaries):

  ```sh
  cargo build --release --features garmin-xhd-output
  ```

- An address on the Garmin Marine Network, `172.16.x.x` with netmask
  `255.240.0.0`, on the interface the plotter is reachable through. If the
  radar is on a different subnet — a Furuno on `172.31.x.x`, say — give the
  same interface both addresses:

  ```text
  enp0s13f0u2
    172.31.254.217/16   Furuno radar
    172.16.254.217/12   Garmin Marine Network
  ```

  Mayara picks the Garmin address on the radar's own interface. When the
  plotter is on an interface of its own, any `172.16.x.x` address will do;
  addresses in `172.17`–`172.31` are skipped, because that is where Docker and
  similar bridges live rather than Garmin equipment.

- Mayara and the plotter on the same network segment. Multicast is sent with a
  TTL of 1, so nothing crosses a router.

The log says which radar is being emulated and from where:

```text
Furuno-1234: Garmin xHD output emulating a GMR xHD on 172.16.254.217
```

## One radar at a time

An emulated xHD occupies the fixed addresses and ports a real one does, so
Mayara emulates exactly one — the first radar it discovers. On a boat with more
than one radar, `--brand furuno` (or `navico`, `raymarine`, …) picks which one
that is. The other radars stay available through Mayara's own GUI and the
Signal K API as usual.

A Garmin radar is never bridged: the plotter can already talk to it directly.
For the same reason, do not enable the feature on a boat that has a real Garmin
radar on the network — the emulated one uses the addresses and ports the real
one does, and the plotter would see the two pictures interleaved.

## What the plotter shows

The emulated radar reports itself as a GMR xHD, so the plotter offers the
controls an xHD has:

| Control       | Effect on the source radar                                     |
| ------------- | -------------------------------------------------------------- |
| Transmit      | Standby / Transmit                                              |
| Range         | Range, snapped by the radar to the nearest range it supports    |
| Gain          | Gain, manual or automatic                                       |
| Sea clutter   | Sea clutter: off, manual, or automatic                          |
| Rain clutter  | Rain clutter: off, or a level. Switching it on uses 50%         |

Those five are all the plotter offers. The emulated radar reports only the
features the bridge can actually pass on, so a control that would do nothing —
timed transmit, no-transmit zones, antenna rotation speed, a second range — is
absent rather than dead. Use Mayara's GUI or the Signal K API for those.

Ranges come from the xHD's own table (1/8 NM to 48 NM). The plotter can only
choose from that list; the radar then picks whatever it supports that is
closest. On a radar set to metric ranges the two ladders do not line up, and
the range ring the plotter draws can be a few percent off. The echoes stay in
the right place regardless — the picture is scaled by the distance the radar
says its samples cover, not by the range the plotter displays. Switching the
radar to nautical ranges in Mayara makes the two agree.

Radars that classify moving targets — Navico Doppler, Furuno Target Analyzer,
Garmin MotionScope — have nowhere to put that on an xHD, which knows only echo
strength. Approaching and receding targets are drawn as strong echoes, and
Furuno's rain classification as a moderate one.

## Trying it without a plotter

A second mayara makes a serviceable stand-in for the chartplotter, on the same
machine. Give the loopback interface an address on the Garmin network, run the
bridge against the built-in emulator, and point a second mayara at it:

```sh
sudo ifconfig lo0 alias 172.16.99.1     # Linux: sudo ip addr add 172.16.99.1/16 dev lo
mayara-server --emulator -i lo0 --port 6602
mayara-server -i lo0 --brand garmin --port 6603
```

The second one logs `Garmin CDM heartbeat: product_id=0x06d0 (GMR xHD)` and
then finds the radar; open its GUI on port 6603 to see the emulator's picture
arriving over the Garmin protocol.

## Troubleshooting

**The plotter never lists the radar.**
Check the log for `Garmin xHD output emulating a GMR xHD on …`. If it says no
address on the Garmin Marine Network was found, the host has no `172.16.x.x`
address. Otherwise, confirm the announcement is going out on the right
interface:

```sh
sudo tcpdump -i <interface> udp port 50050
```

There should be a packet to `239.254.2.2` every second for the first half
minute, then every five seconds.

**The radar is listed but stays blank.**
The source radar has to be transmitting: press Transmit on the plotter, or
check the radar's power state in Mayara's GUI. If it is transmitting, watch the
sweep data with `tcpdump -i <interface> udp port 50102`.

**Range or gain buttons do nothing.**
Run `mayara-server -vv` and watch for `Garmin xHD command:` lines. They log the
command received, the control it was translated into, and any reason the radar
refused it.

## Under the hood

For the protocol details and how the translation works, see
[Garmin xHD Output Bridge](internals/garmin-xhd-output.md).
