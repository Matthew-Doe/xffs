# COW candidate verification — 2026-10-01

Status: **candidate preparation; privileged disposable validation pending**.
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

## Remaining release checks

Minimum-toolchain validation and a fresh source archive build/install/smoke
check are being completed. Their results will be appended before final packaging.

The current privileged disposable gate needs this user-run command, from the
repository root as the normal user:

```sh
python3 scripts/device-validation.py --bin-dir "$PWD/target/release"
```

The agent's `sudo -n true` returned “a password is required.” This gate covers
backend and formatter loops plus device-backed FUSE for both sector sizes.
Historical privileged passes are not counted as a new 0.0.2 run.
