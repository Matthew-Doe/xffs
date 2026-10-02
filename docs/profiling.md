# Write-path profiling

Build optimized binaries with `cargo build --release --workspace --locked`.
The opt-in `mount-xffs --profile-json HOST_FILE` option records aggregate timings
in memory and writes JSON after normal unmount. HOST_FILE must not exist. It is
opened after privilege drop and before mounting. Startup validation/recovery is
excluded. No durability barriers, write ordering or request limits change.

Use the non-formatting workload runner as your normal user for an image:

```sh
python3 scripts/profile-writes.py --output /tmp/xffs-profile-image-new --size-mib 8
```

For the existing serial-bound USB filesystem, run from your normal account:

```sh
sudo python3 scripts/profile-writes.py --device \
  --expect-serial 0085199340190280 \
  --output "$PWD/profile-usb-new" --size-mib 8
```

Use a new host output directory. Device mode never formats, and uses the existing
exclusive-claim/identity/privilege-drop checks. Keep the drive connected. The
runner creates one UUID-named temporary file, verifies an 8 MiB creation and
8 MiB overwrite in separate mounts, removes that file, and verifies restored
free blocks/inodes. Other files are not modified. A failed USB run retains its
test file and evidence for inspection; it does not automatically remove evidence.
Image mode uses a disposable temporary image.

Add `--no-profile` for the identical workload without timing counters. The default
size is 8 MiB per phase, with an explicit 1–64 MiB limit. `--bin-dir` selects the
prebuilt executables. Evidence includes workload elapsed times, counters, mount
and checker logs, cleanup results, and mount binary path/hash on new runs.

## Interpreting the counters

Timings are inclusive and **overlap**: do not add backend, Linux, request and
commit totals together. Within a commit, preflight, data write/flush, journal
write/flush, publish, checkpoint write/flush, and retire are distinct phases.
Publish and retire each contain two control writes and two flushes.

- `request/write`: core write request, including staging and commits.
- `commit/*`: journal transaction phases, including metadata-only transactions.
- `backend/*`: all reads, writes and flushes after startup; includes verification
  reads and small namespace/sync operations outside the timed write loop.
- `linux/identity`: device identity/capacity validation nested within backend I/O.
- `linux/sync_all`: the actual sync syscall, excluding identity validation.
- `staging/*`: allocation and metadata preparation, nested within requests.

Times are nanoseconds. Count and maximum latency are exact; p50/p95 are
power-of-two histogram **upper bounds**, not exact percentiles. Requested byte
counts represent attempted backend transfers, not physical flash traffic.
Error counts apply to backend operations; phase counters do not classify errors.
No per-I/O messages are logged during the workload. Counters are opt-in and have
measurable overhead on fast images.

## USB measurement — 2026-10-02

The user's run is in `profile-usb-20261002`. The
[tracked aggregate](hardware-results/2026-10-02-profile.json) retains exact counters.
The existing 116.1 GiB USB filesystem was not reformatted. Both content checks,
post-unmount filesystem checks and final free-space restoration passed.

| Measurement | Create 8 MiB | Overwrite 8 MiB |
| --- | ---: | ---: |
| Write + fsync wall time | 67.066 s | 52.201 s |
| Throughput | 0.119 MiB/s | 0.153 MiB/s |
| Time inside Linux sync_all | 65.442 s | 50.524 s |
| sync_all / workload wall time | 97.6% | 96.8% |
| Flush calls | 14,403 | 14,396 |
| Maximum single sync_all | 794.157 ms | 793.098 ms |
| Checkpoint flush phase | 21.664 s | 17.025 s |
| Publish controls phase | 20.357 s | 15.594 s |
| Retire controls phase | 15.070 s | 11.359 s |
| Data flush phase | 7.685 s | 5.853 s |
| Journal payload flush phase | 1.370 s | 1.368 s |
| Metadata staging | 0.214 s | 0.204 s |
| Allocation | 0.000162 s | 0.000201 s |

The seven barriers per block transaction dominate. Small metadata transactions,
explicit syncs and FUSE request boundaries explain counts slightly above the
nominal 2,048 transactions × 7 flushes. Core writes observed 16 requests per phase;
transactions numbered 2,057 including create metadata, and 2,056 for overwrite.

Backend writes requested about 112.4 MiB for each 8 MiB application phase
(roughly 14× software-level traffic). Linux caching and device behavior determine
physical traffic, so this is not a flash write-amplification measurement.

The profiled host-backed /tmp image took 0.095 s for creation and 0.115 s for
overwrite. The same unprofiled image workload took 0.081 s and 0.096 s. These
single-run results indicate around 15–20 ms instrumentation cost, orders of
magnitude below the USB flush time; they are not USB throughput measurements.

## Conclusion and next experiment

The measured bottleneck is synchronous durability in the Linux/device storage
path, not allocator or checksum CPU time. These counters do not isolate USB
firmware, NAND operations, or individual kernel block-layer requests.

The next optimization experiment should be bounded multi-block transactions,
keeping COW, data-before-journal ordering, metadata capacity preflight, and
durability guarantees. It needs an explicit transaction/short-write contract and
crash tests before benchmarking. Simply removing flushes would invalidate the
current storage protocol. No such optimization is included in this profiling
change.

Validation: formatting, warnings-denied Clippy, workspace tests, 66 host harness
tests and Python compilation passed. A focused instrumented/uninstrumented
simulator comparison confirmed identical I/O order and error outcomes, including
data-write, data-flush and later journal failures.
