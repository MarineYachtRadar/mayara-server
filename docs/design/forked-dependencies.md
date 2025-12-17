# Forked Dependencies

Mayara uses a few forked dependencies that contain fixes not yet merged upstream. This document explains why these forks exist and their current status.

## Overview

| Dependency | Fork | Branch | Reason |
|------------|------|--------|--------|
| nmea-parser | keesverruijt/nmea-parser | position_precision | High-precision GPS support |
| tokio-tungstenite | keesverruijt/tokio-tungstenite | (default) | Required for permessage-deflate |
| tungstenite-rs | keesverruijt/tungstenite-rs | permessage-deflate | WebSocket compression |

---

## nmea-parser

**Fork:** https://github.com/keesverruijt/nmea-parser
**Branch:** `position_precision`
**Upstream:** https://github.com/zaari/nmea-parser

### Why?

The upstream nmea-parser library doesn't preserve the full precision of GPS coordinates. This matters when parsing NMEA sentences from high-precision GPS receivers like the Furuno SCX20 (via canboat).

Without this fix, position data loses significant decimal places, which is unacceptable for accurate radar overlay positioning.

### Status

The fork is kept in sync with upstream's master branch, plus the precision fixes. A PR to upstream would be welcome if someone wants to champion it.

---

## tungstenite-rs (WebSocket library)

**Fork:** https://github.com/keesverruijt/tungstenite-rs
**Branch:** `permessage-deflate`
**Upstream:** https://github.com/snapview/tungstenite-rs

### Why?

This fork adds support for **permessage-deflate**, a standard WebSocket extension (RFC 7692) that compresses messages. Radar spoke data compresses extremely well, reducing bandwidth significantly.

All modern browsers support permessage-deflate. It's a no-brainer for performance, yet the upstream library doesn't include it.

### Status

Someone submitted a PR over a year ago ([#426](https://github.com/snapview/tungstenite-rs/pull/426)) but it hasn't been merged. The reasons are unclear - possibly maintainer bandwidth or differing opinions on scope.

Until upstream merges this, we maintain our own fork.

---

## tokio-tungstenite

**Fork:** https://github.com/keesverruijt/tokio-tungstenite
**Branch:** (default)
**Upstream:** https://github.com/snapview/tokio-tungstenite

### Why?

This is the async wrapper around tungstenite-rs. Our fork is modified to work with the permessage-deflate branch of tungstenite-rs.

### Status

Depends on the tungstenite-rs fork. If upstream ever merges permessage-deflate support, we can switch back to the official crate.

---

## Possible Future Plans

1. **nmea-parser**: Consider submitting a PR upstream for the precision fixes
2. **tungstenite/tokio-tungstenite**: Monitor the upstream PR. If it gets merged and released, switch to the official crates
3. **Fallback**: If upstream remains stagnant, consider moving forks to the MarineYachtRadar organization for better long-term maintenance

## Updating the Forks

When upstream libraries release important fixes, the forks should be rebased:

```bash
# Example for nmea-parser
git clone https://github.com/keesverruijt/nmea-parser
cd nmea-parser
git remote add upstream https://github.com/zaari/nmea-parser
git fetch upstream
git checkout position_precision
git rebase upstream/master
git push --force-with-lease
```

After rebasing, rebuild mayara-server to verify everything still works.
