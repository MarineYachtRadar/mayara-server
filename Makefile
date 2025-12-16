# Mayara Build System
#
# Usage:
#   make          - Build release with docs (recommended)
#   make release  - Build release with docs
#   make debug    - Build debug with docs
#   make dev      - Build debug with live GUI reload (no embedding)
#   make docs     - Generate rustdoc only
#   make run      - Build and run server
#   make run-dev  - Build and run dev server (live GUI reload)
#   make clean    - Clean build artifacts

.PHONY: all release debug dev docs run run-dev clean test

# Default: build release with embedded docs
all: release

# Generate rustdoc for core and server
docs:
	@echo "Generating rustdoc..."
	cargo doc --no-deps -p mayara-core -p mayara-server
	@echo "Documentation generated at target/doc/"

# Build release binary with docs embedded
release: docs
	@echo "Building release..."
	cargo build --release -p mayara-server
	@echo ""
	@echo "Build complete: target/release/mayara-server"
	@echo "Rustdoc available at: http://localhost:6502/rustdoc/mayara_core/"

# Build debug binary with docs embedded
debug: docs
	@echo "Building debug..."
	cargo build -p mayara-server
	@echo ""
	@echo "Build complete: target/debug/mayara-server"
	@echo "Rustdoc available at: http://localhost:6502/rustdoc/mayara_core/"

# Build debug with dev feature (GUI served from filesystem, no embedding)
# Useful for GUI development - just refresh browser after editing JS/HTML
dev:
	@echo "Building dev (no GUI embedding)..."
	cargo build -p mayara-server --features dev
	@echo ""
	@echo "Build complete: target/debug/mayara-server"
	@echo "GUI served from mayara-gui/ directory (edit and refresh)"

# Build and run the server
run: release
	@echo "Starting server..."
	./target/release/mayara-server

# Build and run dev server (live GUI reload)
run-dev: dev
	@echo "Starting dev server..."
	./target/debug/mayara-server

# Run tests
test:
	cargo test -p mayara-core -p mayara-server

# Clean build artifacts
clean:
	cargo clean
