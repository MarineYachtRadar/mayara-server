FROM rust:1.94-alpine AS builder
# `curl` is needed at build time by utoipa-swagger-ui's build.rs to fetch
# the Swagger UI bundle; without it the build fails with `curl command not
# found`. `perl` is required by openssl-sys.
RUN apk add --no-cache build-base curl perl cmake

WORKDIR /usr/src/mayara-server

# Cache downloaded crates: build.rs requires the full source tree so it fails
# here, but cargo compiles dependencies first, so they are cached.
COPY Cargo.lock Cargo.toml build.rs ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release 2>/dev/null || true \
    && rm -rf src

COPY src src/
# Cargo parses the whole manifest (including `[[example]]` blocks) on every
# build. The generate-fixtures example is gated by `required-features =
# ["pcap-replay"]` and is never built here, but the manifest is invalid
# unless its source file is present.
COPY examples examples/
COPY web web/
RUN cargo build --release

FROM alpine:3.21
LABEL org.opencontainers.image.source="https://github.com/MarineYachtRadar/mayara-server"
LABEL org.opencontainers.image.title="mayara-server"
LABEL org.opencontainers.image.description="Marine radar server with REST API and WebSocket support"
LABEL org.opencontainers.image.license="Apache-2.0"
RUN apk add --no-cache tini \
    && adduser -D -h /home/mayara mayara
COPY --from=builder /usr/src/mayara-server/target/release/mayara-server /usr/local/bin/mayara-server
# Names this deployment in the usage stats a user may agree to send, so a
# container install is not counted as someone running the binary by hand.
# A packager that builds on this image overrides it (the Signal K plugin
# passes signalk-server-plugin).
ENV MAYARA_DEPLOYMENT=container
USER mayara
EXPOSE 6502
# Probe the always-served, unauthenticated discovery doc so the container
# reports healthy once mayara's HTTP server is up. Deliberately checks only
# mayara itself, not the upstream Signal K connection — a boat offline from
# Signal K still runs a healthy radar server. Uses BusyBox wget (already in
# the Alpine base; no curl). The in-container port is always 6502 regardless
# of how Signal K proxies it externally.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:6502/signalk || exit 1
ENTRYPOINT ["tini", "--"]
CMD ["mayara-server"]
