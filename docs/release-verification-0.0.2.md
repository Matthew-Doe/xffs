# 0.0.2 candidate verification — 2026-10-02

Status: **all candidate verification gates passed for the source at commit
`6821cb0e791568122cbb5a5f20751aa36406f8a0`**. This record applies to software
0.0.2 and experimental on-disk revision 2. No tag or publication has been made.
Earlier 0.0.1 results remain in the [historical release record](release-verification.md).

## Checkout checks

Rust 1.98.1 passed formatting, warnings-denied workspace/all-target Clippy,
workspace/all-target tests with the lockfile, an optimized workspace build, and
the simulator crash demonstration. All 23 writable tests passed. Three backend
loop-device tests and one formatter loop-device test are ignored in ordinary
Cargo runs and were exercised separately below. Python 3.14.7 passed all 68 host
harness tests; all Python scripts compiled successfully.

Using the optimized workspace binaries, both image FUSE smoke scripts passed:

- Read-only clean and recovered mounts, including complete-image hash
  preservation.
- Writable terminal operations, nano, durability, open-handle lifetimes,
  permissions, locking, and remount persistence.

The close-error tests require expected disconnect errors to be recorded and
unrelated errors to propagate. Journal-state tests distinguish retirement,
publication, committed, clean and unknown states without claiming physical
data-write interruption.

## Disposable device verification

The user ran the following command from the ordinary account; the script invoked
sudo for the disposable loop-device checks:

```sh
python3 scripts/device-validation.py --bin-dir "$PWD/target/release"
```

All three backend tests passed: capacity change, device contract/exclusion, and
mounted-child refusal. The mounted-child setup printed a partition reread
warning before its fallback and successful result; it was not a failed test.
The loop formatter/reopen test passed.

Device-backed FUSE passed on both 512-byte and 4096-byte loop geometries,
including ownership, privilege dropping, exclusive access, writable workload,
saved-file remount persistence, read-only whole-image hash, unmount and loop
cleanup. For each geometry, final accounting was 7,638 free blocks and 510 free
inodes, compared with 7,640 and 511 before the workload. No physical device was
formatted or unplugged in these checks.

## USB and batching evidence

The earlier physical COW interruption evidence remains in the
[October 1 hardware report](hardware-results/2026-10-01.md). Nine overwrites
were acknowledged and the tenth application write returned EIO during journal
retirement. All replacement and untouched bytes validated, and final checking
and unmount passed. This does not claim that a physical interruption occurred
during data writes.

The [October 2 batching report](hardware-results/2026-10-02-batching.md)
records the rotating 12-run experiment on the existing USB filesystem. At the
64 KiB default, median 8 MiB creation and overwrite improved from 52.249 and
52.293 seconds at 4 KiB to 3.182 and 2.376 seconds. Total flush counts fell
93.31% and 93.36%, exceeding the 90% acceptance threshold. All runs passed
content verification, filesystem checks, cleanup and free-space restoration.
The seven-barrier durability protocol and revision 2 encoding are unchanged.
No additional physical interruption trial was performed.

## Source archive

The previous clean-source archive for commit
`6821cb0e791568122cbb5a5f20751aa36406f8a0` passed checksum verification; its 83
regular files matched tracked Git files exactly, and its embedded Git archive
commit matched the source-commit sidecar. A fresh extraction built all optimized
tools with `--locked`; read-only and writable image FUSE smoke checks and the
crash demonstration passed.

This verification record is committed separately. Regenerate the final archive
from the resulting clean HEAD, then verify its checksum and source-commit
sidecars, exact tracked-file membership, fresh optimized build and image smoke
checks. Keep the archive and its sidecars with the release artifacts. No tag or
upload is part of this verification.
