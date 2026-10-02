# Bounded durable-write batching validation

The default is 64 KiB, with 4/16/64/256 KiB tuning. Format revision 2 and all seven
transaction durability barriers are unchanged. No release version or tag changes
are part of this work.

## Correctness coverage

The simulator derives mutation boundaries from successful traces. Across all
four settings it tests full/partial/unaligned overwrites, sparse extension,
same-block and separate-block EOF-tail replacement, shrink/re-extension, and
requests crossing batch boundaries. Every traced write/flush boundary is faulted
with discarded pending writes, torn prefixes (1, 80, 2,048, 4,095 and 4,096 bytes),
and selective persistence of alternating pending writes. Recovered contents must
match a prefix of whole batches. Acknowledged writes must survive. Reopening,
allocation ownership validation, continued writes and reclamation are checked.
Recovery of a committed 64 KiB overwrite is itself interrupted at every boundary.

Aligned 1 MiB creation and overwrite tests assert completed batches, bytes and
flush counts at every setting; 64 KiB requires 16 batches / 112 flushes, versus
256 / 1,792 at 4 KiB. A separate test checks the unset default. Append requests
remain independent. The 1 MiB request limit and invalid settings are tested.

Near-full fixtures cover retained reduction to one block, one-spare-block
multi-batch overwrite, and separate EOF-tail updates that fail with one spare
block and succeed with two. Fragmented fixtures exercise extra overflow-metadata
space, extent, transaction-image and memory limits, checking zero mutation on
rejection and continued usability. The historical short-write fixture explicitly
selects 4 KiB. Reductions are counted independently of successful data batches.

## Checks

- Formatting and warnings-denied workspace/all-target Clippy passed with Rust
  1.98.1 and Rust 1.89.0. The 1.89 toolchain was downloaded from the official
  distribution, checksum-verified, and installed only under `/tmp`.
- Full workspace/all-target tests passed on Rust 1.98.1 (debug) and Rust 1.89.0
  (release), including all 23 writable tests. Four privileged tests excluded
  from ordinary runs were subsequently passed in the terminal run below.
- Host Python harness tests: 68 passed; modified Python scripts compile.
- The complete 12-run rotating image profiling experiment passed contents,
  filesystem checks, cleanup and restored free-space accounting. These timings
  are host-image validation only.
- The retained simulator crash demonstration passed.
- Read-only and writable image FUSE scripts passed outside the sandbox, including
  complete read-only image hashes, nano, durability, lifetimes and remount checks.
- The user ran `python3 scripts/device-validation.py --bin-dir target/release`
  in a terminal: all three backend tests and the formatter test passed, followed
  by successful 512-byte and 4,096-byte disposable loop-device FUSE checks.
  Physical devices were not formatted. A repeat exposed transient `LockContention`
  while reopening the disposable loop after writes. The formatter test now retries
  only this claim error for at most 15 seconds, matching the existing device
  harness approach; production device claiming is unchanged. The corrected privileged
  suite passed in full before USB profiling.

## Performance acceptance

The rotating experiment in `scripts/profile-batches.py` passed all twelve USB
runs. At 64 KiB, median 8 MiB creation improved from 52.249 to 3.182 seconds and
overwrite from 52.293 to 2.376 seconds. Total flushes dropped 93.31% and 93.36%,
exceeding 90% in both phases. All contents, consistency checks, cleanup and
free-space restoration passed. The default remains 64 KiB.

[USB results and evidence](hardware-results/2026-10-02-batching.md) retain all
four tuning settings, three repetitions, the same-binary baseline, counters,
phase timings, latency bounds, throughput and provenance. No physical formatting,
release publication or additional unplug trial was performed.
