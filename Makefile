# Mayara Build System
#
# Usage:
#   make          - Build release with docs (recommended)
#   make release  - Build release with docs
#   make debug    - Build debug with docs
#   make test     - Build and run tests
#   make docs     - Generate rustdoc only
#   make run      - Build and run server
#   make docker   - Build the Docker image
#   make demo     - Rebuild the docker demo image
#   make clean    - Clean build artifacts

.PHONY: all release debug docs run clean test fixtures docker demo changelog

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

# Build and run the server
run: release
	@echo "Starting server..."
	./target/release/mayara-server

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

# Optional per-developer extensions: targets that should never be committed
# (deploy shortcuts to your own boxes, scratch experiments, etc.). The leading
# dash makes the include silent when the file is absent, so a fresh clone
# behaves identically to having no file. `Makefile.local` is gitignored.
-include Makefile.local
