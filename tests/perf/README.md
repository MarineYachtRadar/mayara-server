# Mayara performance test harness

Reproducible CPU profiling of `mayara-server` under different workloads
and configurations. The harness spawns mayara under [`samply`](https://github.com/mstange/samply),
drives it with the Python `spoke_viewer` client in load-driver mode,
and saves labelled profile files for later side-by-side comparison.

Designed to run unchanged on macOS (Apple Silicon) and on the Pi
(`aarch64-unknown-linux-musl`).

## Prerequisites

```sh
cargo install samply
make fixtures          # generates testdata/pcap/*.pcap.gz
```

Build the mayara binary you want to profile with debug symbols and a
**statically linked** zlib so the `deflate_slow` / `longest_match`
hot path symbolicates on every platform:

```sh
LIBZ_SYS_STATIC=1 CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    cargo build --release --features pcap-replay
```

### Linux only: `perf_event_paranoid`

`samply` needs unprivileged `perf_event_open`. On most distros that
means dropping `/proc/sys/kernel/perf_event_paranoid` to 1 or lower:

```sh
sudo sysctl kernel.perf_event_paranoid=1
```

`run.sh` will refuse to start if this isn't set.

## One-off run

```sh
./tests/perf/run.sh
```

Default: profiles all four workloads (emulator, navico, furuno, raymarine)
× both compression modes (deflate on/off), 30 s per run, saves into
`tests/perf/results/`. The results directory is gitignored.

Then:

```sh
python3 ./tests/perf/analyze.py tests/perf/results/*.json.gz > tests/perf/results/summary.md
```

## Comparing two git refs

Typical use: confirm that a perf commit actually moved the numbers in
the expected direction.

```sh
# Build "before" binary at the parent of the perf commit
git checkout <commit>~1
LIBZ_SYS_STATIC=1 CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    cargo build --release --features pcap-replay
cp target/release/mayara-server /tmp/mayara-before

# Build "after" binary at the perf commit (or any later branch)
git checkout <commit>      # or the branch you're on
LIBZ_SYS_STATIC=1 CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    cargo build --release --features pcap-replay
cp target/release/mayara-server /tmp/mayara-after

# Profile both — same workload, two binaries
./tests/perf/run.sh --binary /tmp/mayara-before --label before
./tests/perf/run.sh --binary /tmp/mayara-after  --label after

# Compare (markdown to stdout)
python3 ./tests/perf/analyze.py tests/perf/results/before-*.json.gz \
                                tests/perf/results/after-*.json.gz
```

If the `<commit>~1` checkout doesn't have this scaffold yet, copy the
two files out before switching:

```sh
cp tests/perf/run.sh tests/perf/analyze.py /tmp/
```

then invoke `/tmp/run.sh` instead. It uses absolute paths to the
in-tree fixtures and client, so it works from anywhere as long as the
working tree it's pointed at is a mayara checkout.

## Options

```
--binary PATH         mayara-server binary (default: target/release/mayara-server)
--label NAME          prefix for output filenames (default: HEAD)
--output-dir DIR      output directory (default: tests/perf/results)
--workloads LIST      comma-separated subset of: emulator,navico,furuno,raymarine
                      (default: all four)
--compression MODE    both | on | off (default: both)
--duration SECONDS    client read duration per run (default: 30)
--port PORT           mayara listen port (default: 6504)
--clients N           parallel clients per run (default: 1)
```

## Caveats

- **Emulator dominates the profile** at ~50% self-time in
  `EmulatorReportReceiver::generate_spoke_batch` — useful as a
  high-rate spoke source for stressing the WebSocket / compression
  / serialization path, not for measuring `BlobDetector` or
  `process_frame`.
- **`samply` on macOS samples sleeping threads** alongside running
  ones; `analyze.py` filters out stacks containing `_pthread_cond_wait`
  or `park_internal` so the headline percentages compare like-for-like
  with the Pi numbers.
- **Mac CPU is much faster than the Pi**, so per-leaf percentages
  don't translate one-to-one between hosts. Compare same-host
  before/after, not Mac-vs-Pi absolutes.
- **Pcap workloads have a finite spoke window.** The fixtures in
  `testdata/pcap/` are small (10-25 KB), so spoke traffic on the
  client lasts seconds rather than the full `--duration`. Increase
  the fixture or use `--emulator` for long sustained load.
