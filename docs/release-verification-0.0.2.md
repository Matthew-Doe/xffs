# COW candidate verification — 2026-10-01

Status: **COW milestone complete; all required candidate checks passed**.
This record applies to software 0.0.2 and unchanged on-disk revision 2.
No tag or publication has been made. Earlier 0.0.1 results remain in the
[historical release record](release-verification.md).

## Completed checkout checks

Rust 1.98.1 passed formatting, warnings-denied workspace/all-target Clippy,
workspace/all-target tests with the lockfile, and optimized workspace build.
All 18 writable tests passed; four explicitly privileged loop-device tests
remain excluded from ordinary cargo test. All 66 Python host harness tests and
Python compilation passed. The packager correctly rejected a dirty tracked tree.

Using the optimized executables, these passed:

- Read-only image FUSE smoke, including unchanged complete images.
- Writable image FUSE smoke, including nano and remount.
- Normal acceptance image workload, repeated COW writes and verification.
- Resumed acceptance image workload, preserving existing files.

The close-error tests require expected disconnect errors to be recorded and
unrelated errors to propagate. Journal-state tests distinguish retirement,
publication, committed, clean and unknown states without claiming physical
data-write interruption.

## Physical evidence

The user's completed USB run is recorded in the
[October 1 hardware report](hardware-results/2026-10-01.md), with hashes of the
retained source evidence. Nine overwrites were acknowledged and the tenth
application write returned EIO during journal retirement. All replacement and
untouched bytes validated, and final checking/unmount passed. Earlier
initialization-only interruption evidence is preserved and explicitly limited.

No new physical-device test is needed to validate the reporting-only fixes;
this candidate makes no additional hardware or performance claim.

## Minimum toolchain

Rust 1.89.0 was installed only under `/tmp/xffs-cow-rust-1.89`. Formatting,
warnings-denied Clippy, workspace/all-target tests and the simulator crash
demonstration all passed with the lockfile. The system toolchain was unchanged.
The minimum-toolchain log is retained at `/tmp/xffs-cow-msrv-verification.log`.

## User-run privileged gate

The user ran the current 0.0.2 release binaries through:

```sh
python3 scripts/device-validation.py --bin-dir "$PWD/target/release"
```

All three backend tests passed: capacity change, device contract/exclusion, and
mounted-child refusal. The mounted-child setup printed a partition reread
warning before its fallback and successful result; that warning is not a failed
formatter test. The separate loop formatter/reopen test passed.

Device-backed FUSE passed on both 512-byte and 4096-byte loop geometries:
ownership, privilege dropping, exclusive access, writable workload, saved-file
remount persistence, read-only whole-image hash, unmount and loop cleanup.
For each geometry the final retained-file accounting was 7,638 free blocks and
510 free inodes, compared with 7,640 and 511 before the workload. The harness
reported all disposable tests passed. This resolves the initial noninteractive
sudo prerequisite; no additional USB formatting or unplug trial was performed.

## Source archive and installation

The clean candidate archive passed SHA-256 verification, exact comparison of
regular archive entries with tracked Git files, and pax/source-commit sidecar
comparison. No untracked USB logs or build artifacts were included.

A fresh extraction built all optimized tools with Rust 1.89.0 and the lockfile.
Both image FUSE smoke scripts and all 66 host harness tests passed there.
All six executables were installed into a temporary prefix and compared with
the build outputs. Installed mkfs/check created and validated a 16 MiB image;
removing the installed tools emptied bin while retaining that image.

The final clean-HEAD archive is regenerated after this verification record is
committed. Archive sidecars record the exact commit and SHA-256, avoiding a
self-referential commit hash here. The artifact and external verification logs
are under `/tmp/xffs-cow-release`; the final archive is freshly extracted,
built and smoke-tested again. No tag or upload is part of this milestone.
