# Contributing to Mayara

Thanks for your interest in contributing! This guide will help you get started.

## Getting Started

1. Fork the repository and clone your fork
2. Create a feature branch from `main`:
   ```bash
   git checkout -b my-feature-name
   ```
3. Make your changes
4. Push to your fork and open a Pull Request

## Before You Code

Please read through the relevant documentation first:

- **[Architecture Overview](docs/design/architecture.md)** - Understand how mayara-core, mayara-server, and mayara-gui fit together. The key principle: mayara-core is the single source of truth for all radar logic.

- **[Getting Started (Developer Guide)](docs/develop/getting_started.md)** - Quick orientation for new developers, including how to run the server and navigate the codebase.

- **[Adding Radar Models](docs/develop/adding_radar_models.md)** - If you're adding support for new hardware.

- **[API Naming Conventions](docs/radar-API/signalk_radar_api_naming.md)** - Use semantic names (what the control *does*), not vendor marketing names.

- **Protocol Documentation** - Brand-specific protocol details live in `docs/radar-protocols/{brand}/`.

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Add tests for new functionality

## Pull Request Process

1. Keep PRs focused - one feature or fix per PR
2. Update documentation if you're changing behavior
3. Make sure `cargo test --workspace` passes
4. Write a clear PR description explaining what and why

## Questions?

Open an issue if something's unclear or you want to discuss an approach before diving in.
