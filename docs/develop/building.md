# Building Mayara Server

This guide explains how to build and run mayara-server for different scenarios.

---

## TL;DR - Quick Reference

```bash
cd /home/dirk/dev/mayara-server

# Production build (GUI embedded in binary)
make release
./target/release/mayara-server

# Development build (edit GUI files, refresh browser)
make run-dev

# Debug with verbose output
RUST_BACKTRACE=1 cargo run -p mayara-server --features dev
```

---

## Understanding the Two Build Modes

### Mode 1: Production (GUI Embedded)

The GUI is **downloaded from npm** and **compiled into the binary**. Result: single self-contained executable.

```bash
make release
# or manually:
cargo build --release -p mayara-server
```

**What happens:**
1. `build.rs` runs `npm install @marineyachtradar/mayara-gui@latest`
2. GUI files are copied to `$OUT_DIR/gui/`
3. `rust-embed` compiles all files into the binary
4. Binary is ~30-40MB but fully self-contained

**Use when:** Deploying, distributing, running without GUI source

---

### Mode 2: Development (GUI from Filesystem)

The GUI is **served from the filesystem**. Edit JS/HTML/CSS, refresh browser - no rebuild needed.

```bash
make run-dev
# or manually:
cargo build -p mayara-server --features dev
./target/debug/mayara-server
```

**What happens:**
1. `build.rs` skips npm download
2. Server serves files directly from `../mayara-gui/` directory
3. No GUI embedded - binary is smaller and builds faster

**Use when:** Developing the GUI, testing changes quickly

**Requirement:** Clone mayara-gui as sibling directory:
```bash
cd /home/dirk/dev
git clone <mayara-gui-repo> mayara-gui
```

---

## All Build Commands

### Using Make (Recommended)

```bash
cd /home/dirk/dev/mayara-server

make              # Build release with docs (same as: make release)
make release      # Build release binary, GUI embedded
make debug        # Build debug binary, GUI embedded
make dev          # Build debug binary, GUI from filesystem
make run          # Build release and run
make run-dev      # Build dev and run
make docs         # Generate rustdoc only
make test         # Run tests
make clean        # Clean build artifacts
```

### Using Cargo Directly

```bash
cd /home/dirk/dev/mayara-server

# Release build (production)
cargo build --release -p mayara-server
./target/release/mayara-server

# Debug build
cargo build -p mayara-server
./target/debug/mayara-server

# Dev build (GUI from filesystem)
cargo build -p mayara-server --features dev
./target/debug/mayara-server

# With specific port
./target/debug/mayara-server -p 6502

# With verbose logging
RUST_LOG=debug cargo run -p mayara-server --features dev

# With backtrace on panic
RUST_BACKTRACE=1 cargo run -p mayara-server --features dev
```

---

## Feature Flags

| Feature | Default | Purpose |
|---------|---------|---------|
| `navico` | Yes | Navico radar support |
| `furuno` | Yes | Furuno radar support |
| `raymarine` | Yes | Raymarine radar support |
| `garmin` | No | Garmin radar support |
| `dev` | No | Serve GUI from filesystem + enable Protocol Debugger |
| `rustdoc` | No | Embed rustdoc at `/rustdoc/` endpoint |

```bash
# Enable Garmin support
cargo build -p mayara-server --features garmin

# Disable Navico (faster compile for testing)
cargo build -p mayara-server --no-default-features --features furuno,raymarine

# Multiple features
cargo build -p mayara-server --features dev,garmin
```

---

## Runtime Options

### Brand Restriction (`--brand`)

In production, mayara-server discovers radars from all supported brands. For development or testing with a specific radar, use the `--brand` flag to limit discovery to a single brand:

```bash
# Only discover Furuno radars
./mayara-server --brand furuno

# Only discover Navico radars
./mayara-server -b navico
```

**Available brands:**

| Brand | Flag value | Notes |
|-------|------------|-------|
| Furuno | `furuno` | Uses 172.31.x.x subnet |
| Navico | `navico` | Includes Simrad, B&G, Lowrance |
| Raymarine | `raymarine` | |
| Garmin | `garmin` | Requires `--features garmin` at compile time |

Without `--brand`, all compiled-in brands are discovered. This is the production default.

### Other Useful Options

```bash
# Specify HTTP port (default: 6502)
./mayara-server -p 8080

# Limit to specific network interface
./mayara-server --interface eth0

# Verbose logging
./mayara-server -v      # info + warn
./mayara-server -vv     # debug
./mayara-server -vvv    # trace

# Multi-radar mode (keep looking after first radar found)
./mayara-server --multiple-radar
```

### Protocol Debugging (`--features dev`)

The dev feature enables the Protocol Debugger, a real-time tool for analyzing radar protocol traffic. This is essential for reverse-engineering unknown protocol elements.

```bash
# Run with protocol debugger enabled
cargo run -p mayara-server --features dev
```

When enabled, the server exposes additional debug endpoints:
- `GET /v2/api/debug` - WebSocket for real-time debug events
- `GET /v2/api/debug/events` - Query historical events
- `POST /v2/api/debug/recording/start` - Start session recording
- `POST /v2/api/debug/recording/stop` - Stop and save recording
- `GET /v2/api/debug/recordings` - List saved recordings

See [Protocol Debugger User Guide](../user-guide/protocol-debugger.md) for usage.

---

## Debugging Builds

### Verbose Output

```bash
# See what's happening during build
cargo build -p mayara-server -v

# Very verbose
cargo build -p mayara-server -vv

# With timing info
cargo build -p mayara-server --timings
```

### Common Issues

**Build fails with npm error:**
```bash
# Clear npm cache and rebuild
npm cache clean --force
rm -rf target
cargo build --release -p mayara-server
```

**GUI not updating in dev mode:**
- Make sure `--features dev` is set
- Check that `../mayara-gui/` directory exists
- Verify the server output says "GUI served from mayara-gui/ directory"

**"No radars found" at runtime:**
- Check network interface has correct IP (172.31.x.x for Furuno)
- Try running with `RUST_LOG=mayara_core=debug cargo run ...`

---

## Project Layout

```
/home/dirk/dev/
├── mayara-server/           # This repository
│   ├── mayara-core/         # Platform-independent radar library
│   ├── mayara-server/       # The server binary
│   ├── docs/                # Documentation
│   └── Makefile             # Build commands
│
└── mayara-gui/              # GUI repository (sibling, for dev mode)
    ├── index.html
    ├── viewer.html
    ├── control.html
    └── *.js, *.css
```

---

## Logging

```bash
# Specific module
RUST_LOG=mayara_server::brand::furuno::data=debug cargo run -p mayara-server

# Multiple modules
RUST_LOG=mayara_server::web=debug,mayara_core::locator=debug cargo run -p mayara-server

# Everything (very verbose)
RUST_LOG=debug cargo run -p mayara-server

# Save to file for analysis
RUST_BACKTRACE=1 cargo run -p mayara-server --features dev 2>&1 | tee server.log
grep -i "error\|failed" server.log
```

---

## Clean Rebuild

When things get weird:

```bash
cd /home/dirk/dev/mayara-server

# Full clean
cargo clean
rm -rf target

# Rebuild from scratch
make release
# or
make run-dev
```

---

## Summary

| I want to... | Command |
|--------------|---------|
| Build for production | `make release` |
| Develop the GUI | `make run-dev` |
| Debug with logs | `RUST_LOG=debug make run-dev` |
| Clean rebuild | `make clean && make release` |
| Run tests | `make test` |

---

See also:
- [Getting Started](getting_started.md) - Full development guide
- [Architecture](../design/architecture.md) - System design
