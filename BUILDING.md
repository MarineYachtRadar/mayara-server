# Building from Source

This guide covers installing Rust and building mayara-server on Windows, Linux, and macOS.

## Prerequisites

### All Platforms

- Git (for cloning the repository)
- Internet connection (for downloading dependencies)

## Installing Rust

### Official Method (Recommended)

#### Linux / macOS

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the prompts and select the default installation. Then reload your shell:

```bash
source $HOME/.cargo/env
```

#### Windows

1. Download [rustup-init.exe](https://rustup.rs/)
2. Run the installer
3. Follow the prompts (default options are fine)
4. Restart your terminal/PowerShell

### Verify Installation

```bash
rustc --version
cargo --version
```

You should see version 1.90 or later.

### Updating Rust

```bash
rustup update
```

## Building mayara-server

### Clone the Repository

```bash
git clone https://github.com/MarineYachtRadar/mayara-server.git
cd mayara-server
```

### Debug Build (faster compilation, slower runtime)

```bash
cargo build
```

Binary location: `target/debug/mayara-server`

### Release Build (slower compilation, optimized runtime)

```bash
cargo build --release
```

Binary location: `target/release/mayara-server`

### Run Directly

```bash
# Debug build
cargo run

# Release build
cargo run --release

# With arguments
cargo run --release -- --emulator -vv
```

## Feature Flags

mayara-server supports optional features that can be enabled at build time:

```bash
# Build with all default features
cargo build --release

# Build with specific radar brands only
cargo build --release --no-default-features --features navico,garmin
```

Available features:
- `navico` - Navico radar support (BR24, 3G, 4G, Halo)
- `furuno` - Furuno radar support
- `garmin` - Garmin radar support (HD, xHD)
- `raymarine` - Raymarine radar support
- `emulator` - Built-in radar emulator

## Running Tests

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_name
```

## Cross-Compilation

### Using Cross (Docker-based)

```bash
# Install cross
cargo install cross

# Build for various targets
cross build --release --target armv7-unknown-linux-gnueabihf
cross build --release --target aarch64-unknown-linux-gnu
```

## Troubleshooting

### OpenSSL errors on Linux

```bash
# Install OpenSSL development package
sudo apt install libssl-dev pkg-config
```

### Linker errors on Windows

Ensure Visual Studio Build Tools are installed with C++ workload.

### "rustc version X required" error

```bash
rustup update
```

### Out of memory during build

Limit parallel jobs:

```bash
cargo build --release -j 2
```

## IDE Support

### Visual Studio Code

Install the [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer) extension.

### IntelliJ IDEA / CLion

Install the [Rust plugin](https://plugins.jetbrains.com/plugin/8182-rust).

### Vim/Neovim

Use rust-analyzer with your LSP client of choice.
