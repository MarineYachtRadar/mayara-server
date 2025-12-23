# Multicast Socket Architecture

This document explains how mayara handles multicast networking across different platforms and why certain design decisions were made.

## Platform Differences

Marine radars communicate using UDP multicast. The way multicast sockets are configured varies significantly between platforms:

### Linux/Unix

On Linux, we bind directly to the multicast address (e.g., `239.255.0.2:6678`). This provides natural packet filtering - the kernel only delivers packets destined for that specific multicast group.

Additionally, we set `IP_MULTICAST_ALL=0` via `setsockopt()`. Without this, Linux would deliver multicast packets to all sockets bound to the same port, regardless of which multicast groups they joined. This is particularly important on systems with multiple network interfaces where different radars may use the same port but different multicast groups.

### Windows

Windows requires a different approach. According to [Microsoft documentation](https://msdn.microsoft.com/en-us/library/windows/desktop/ms737550), multicast sockets must bind to `0.0.0.0` (INADDR_ANY), not to the multicast address directly. The multicast group membership is then established via `join_multicast_v4()`.

### WASM (Node.js)

The WASM plugin runs inside SignalK Server, which uses Node.js. Node.js's `dgram` module has its own constraints:

- Sockets must bind to `0.0.0.0` for multicast reception
- Group membership is managed via `addMembership()`
- There's no direct access to low-level socket options like `IP_MULTICAST_ALL`

This is handled automatically by the SignalK socket manager - the WASM plugin doesn't need special configuration.

## Native vs WASM Architecture

The key insight is that **native and WASM use completely separate socket implementations**:

```
Native (mayara-server)          WASM (mayara-signalk-wasm)
        │                               │
        ▼                               ▼
   socket2 crate                  SignalK FFI
        │                               │
        ▼                               ▼
   System sockets               Node.js dgram
   (platform-specific)          (always 0.0.0.0)
```

This means:
- Native code can use platform-optimal binding (multicast address on Linux, 0.0.0.0 on Windows)
- WASM automatically gets the correct behavior through Node.js
- No code sharing conflicts between the two implementations

## Multi-NIC Support

Marine installations often have multiple network interfaces - one for the ship's network and separate dedicated interfaces for each radar brand. By default, the OS chooses a single interface for multicast, which may not be the correct one for the radar.

### Joining Multicast on All Interfaces

At startup, mayara-server discovers all non-loopback IPv4 interfaces and joins multicast groups on each one:

```rust
let interfaces = find_all_interfaces();  // Discovers all NICs
for iface in &interfaces {
    locator.add_multicast_interface(iface);
}
```

This ensures radar beacons are received regardless of which physical interface the radar is connected to.

### Interface Selection for Radar Communication

When sending commands to a radar, the correct NIC must be selected. The `find_nic_for_radar()` function uses this priority:

1. **Subnet match**: Find a NIC in the same subnet as the radar IP
2. **Link-local handling**: For 169.254.x.x addresses (common with Navico):
   - Prefer the 172.31.x.x interface (dedicated radar subnet)
   - Fall back to wired ethernet interfaces (`eth*` or `en*`)
3. **Final fallback**: First non-loopback interface

This logic ensures that dedicated radar NICs are preferred over general-purpose network interfaces.

## The IP_MULTICAST_ALL Socket Option

On Linux, when multiple sockets are bound to the same port, the kernel normally delivers multicast packets to all of them. The `IP_MULTICAST_ALL` option controls this:

- `IP_MULTICAST_ALL=1` (default): Deliver to all sockets on this port
- `IP_MULTICAST_ALL=0`: Only deliver to sockets that explicitly joined this multicast group

We set this to 0 via `setsockopt()` immediately after joining a multicast group because:
1. Multiple radar brands may use the same port with different multicast groups
2. On multi-NIC systems, we want interface-specific multicast reception
3. It prevents cross-brand beacon routing (e.g., Navico packets being delivered to Furuno sockets)
4. It reduces unnecessary packet processing

## Navico-Specific Considerations

Navico radars are more complex than other brands because they use **three separate network addresses** for different purposes:

| Purpose | Address Type | Example |
|---------|--------------|---------|
| Spoke data | Multicast | `239.255.0.2:6678` |
| Status reports | Multicast | `239.238.55.73:7527` |
| Commands | Unicast to radar | `192.168.1.100:6680` |

These are completely different IP addresses, not just different ports on the same IP. The beacon packets from Navico radars contain all three addresses, which must be extracted and used appropriately.

### Dual-Range Radars

Navico 4G and HALO radars support dual-range operation, where a single physical radar provides two independent "virtual" radars (Range A and Range B). Each range has its own set of three addresses. The radar sends a single beacon containing both sets of endpoints.

From the software perspective, these appear as two separate radars with suffixes:
- `Navico-ABC123-A` (long range)
- `Navico-ABC123-B` (short range)

Each operates independently with its own controls, though some physical limitations apply (e.g., rotation speed affects both ranges).

## Summary

| Platform | Bind Address | Multicast Join | Notes |
|----------|--------------|----------------|-------|
| Linux | Multicast addr | After bind, on all NICs | IP_MULTICAST_ALL=0 required |
| macOS | Multicast addr | After bind, on all NICs | Similar to Linux |
| Windows | 0.0.0.0 | Before bind, on all NICs | MSDN requirement |
| WASM/Node.js | 0.0.0.0 | Via addMembership | Handled by SignalK |

The native server (`mayara-server`) and WASM plugin (`mayara-signalk-wasm`) use separate socket implementations, so each platform gets optimal behavior without code conflicts.

### Key Implementation Files

- [core_locator.rs](../../mayara-server/src/core_locator.rs) - `find_all_interfaces()` discovers NICs, multicast setup at startup
- [network/mod.rs](../../mayara-server/src/network/mod.rs) - `find_nic_for_radar()` selects correct interface for outbound traffic
- [tokio_io.rs](../../mayara-server/src/tokio_io.rs) - `IP_MULTICAST_ALL` socket option configuration
