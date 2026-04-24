# Mayara Build System
#
# GUI is downloaded from npm (@marineyachtradar/mayara-gui) during build.
# For GUI development, clone mayara-gui repo as sibling and use 'make dev'.
#
# Usage:
#   make          - Build release with docs (recommended)
#   make release  - Build release with docs
#   make debug    - Build debug with docs
#   make test     - Build and run tests
#   make docs     - Generate rustdoc only
#   make run      - Build and run server
#   make run-dev  - Build and run dev server (live GUI reload)
#   make mpi      - Build for linux and deploy and run on merrimac-pi
#   make docker   - Build the Docker image
#   make demo     - Rebuild the docker demo image
#   make clean    - Clean build artifacts

.PHONY: all release debug dev docs run run-dev clean test fixtures docker demo changelog

# Default: build release with embedded docs
all: release

# Generate rustdoc for core and server
docs:
	@echo "Generating rustdoc..."
	cargo doc --no-deps
	@echo "Documentation generated at target/doc/"

# Build release binary with docs embedded
release: docs
	@echo "Building release..."
	cargo build --release 
	@echo ""
	@echo "Build complete: target/release/mayara-server"
	@echo "Rustdoc available at: http://localhost:6502/rustdoc/mayara_core/"

# Build debug binary with docs embedded
debug: docs
	@echo "Building debug..."
	cargo build
	@echo ""
	@echo "Build complete: target/debug/mayara-server"
	@echo "Rustdoc available at: http://localhost:6502/rustdoc/mayara_core/"

# Build release binary with docs embedded
mpi: 
	@echo "Building and running on merrimac-pi..."
	cargo build --release --target aarch64-unknown-linux-musl
	ssh merrimac-pi killall -9 mayara-server || :
	scp target/aarch64-unknown-linux-musl/release/mayara-server merrimac-pi:
	ssh -L 6502:localhost:6502 merrimac-pi RUST_BACKTRACE=full ./mayara-server --pass-ais

mpid:
	@echo "Building and running with debug log on merrimac-pi..."
	cargo build --release --target aarch64-unknown-linux-musl
	ssh merrimac-pi killall -9 mayara-server || :
	scp target/aarch64-unknown-linux-musl/release/mayara-server merrimac-pi:
	ssh -L 6502:localhost:6502 merrimac-pi RUST_BACKTRACE=full ./mayara-server -v --transmit --pass-ais

mpit:
	@echo "Building and running with trace log on merrimac-pi..."
	cargo build --release --target aarch64-unknown-linux-musl
	ssh merrimac-pi killall -9 mayara-server || :
	scp target/aarch64-unknown-linux-musl/release/mayara-server merrimac-pi:
	ssh -L 6502:localhost:6502 merrimac-pi RUST_BACKTRACE=full ./mayara-server -v -v --transmit --pass-ais

# Build debug with dev feature (GUI served from filesystem, no embedding)
# Useful for GUI development - just refresh browser after editing JS/HTML
dev:
	@echo "Building dev (no GUI embedding)..."
	cargo build --features dev
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

# Pcap fixture files used by replay integration tests
FIXTURES = \
	testdata/pcap/furuno-drs4dnxt.pcap.gz \
	testdata/pcap/garmin-xhd.pcap.gz \
	testdata/pcap/navico-4g.pcap.gz \
	testdata/pcap/navico-br24.pcap.gz \
	testdata/pcap/navico-halo20plus.pcap.gz \
	testdata/pcap/navico-halo24.pcap.gz \
	testdata/pcap/navico-halo3006.pcap.gz \
	testdata/pcap/raymarine-quantum.pcap.gz

# (Re)generate pcap fixtures from radar-recordings repo
fixtures: $(FIXTURES)
$(FIXTURES) &:
	cargo run --features pcap-replay --example generate-fixtures

# Run unit tests and integration tests (starts emulator, runs tests, stops server)
test:
	./tests/run-integration.sh

# Docker image
docker:
	docker buildx build -f docker/Dockerfile -t ghcr.io/marineyachtradar/mayara-server:latest .

# Docker demo
demo:
	./demo/build.sh

# Generate changelog (requires git-cliff: cargo install git-cliff)
changelog:
	git-cliff --output CHANGELOG.md
	cat CHANGELOG.manual.md >> CHANGELOG.md

# Clean build artifacts
clean:
	cargo clean
