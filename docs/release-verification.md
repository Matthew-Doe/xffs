# Source release verification — 2026-09-29

Status: **required checks passed; ready for a separate tagging/publishing step**.
No tag or publication has been made. The historical USB
[acceptance evidence](hardware-results/2026-09-29.md) is unchanged and complete.
The historical loop formatter ioctl failure and unreached device FUSE suite are
not counted as passes: the corrected disposable suites must be run explicitly.

## Current candidate checks

From the checkout, Rust 1.98.1 passed:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo run -p xffs-sim --example crash_demo --locked
python3 -m unittest discover -s tests/hardware -v
python3 -m py_compile scripts/*.py
cargo build --release --workspace --locked
```

All 19 Python harness tests passed. Ordinary workspace tests exclude four
explicitly ignored privileged loop tests. New deterministic tests cover transient
contention, the 15-second timeout, full identity comparison including changed disk
sequence, permanent errors, and independent claim deadlines.

The release-binary read-only image smoke passed outside the sandbox. Its initial
sandbox attempt returned 77 because `/dev/fuse` was unavailable; that is retained
as an unmet prerequisite, resolved by the subsequent host run. No sudo or pkexec
was invoked by the agent.

Rust 1.89 checks, fresh archive build/image tests, checksum/content verification
and temporary-prefix installation/removal are recorded below as they complete.

## Initial privileged gate (resolved below)

User-run command, from the checkout after building release executables:

```sh
python3 scripts/device-validation.py --bin-dir "$PWD/target/release"
```

Pending: backend loop tests, corrected formatter partition-refresh tests for
512/4096-byte sectors, and device-backed FUSE ownership, privilege dropping,
exclusion, persistence, read-only preservation and teardown. The corrected
formatter uses `--partscan` and starts with an old DOS partition table; its kernel
child must exist before formatting and disappear afterward. Ioctl errors are
fatal, not suppressed. Do not tag or publish while this gate is pending.

## Minimum toolchain

Rust 1.89.0 was installed under `/tmp/xffs-rust-1.89` without changing the system
toolchain. With that directory's `bin` prepended to PATH, formatting, Clippy with
warnings denied, all workspace/all-target tests and the crash demonstration all
passed with `--locked` where applicable, using the commands above. The source
packager also correctly refused the dirty tracked preparation tree.

## Archive and installation checks

The initial clean source archive used commit
`9a261a61e68b79bf644aef5057f87c64abad215d`. Its SHA-256 check passed; tar file
entries exactly matched `git ls-files`, and its pax source commit matched HEAD.
Cargo.lock, LICENSE, docs, tests and scripts were included; build outputs and
untracked hardware logs were absent. It was extracted into a fresh temporary
directory and these commands passed:

```sh
PATH=/tmp/xffs-rust-1.89/bin:$PATH cargo build --release --workspace --locked
./scripts/mount-smoke.sh --bin-dir "$PWD/target/release"
./scripts/writable-smoke.sh --bin-dir "$PWD/target/release"
```

Both FUSE scripts ran on the host outside the sandbox, without sudo. Read-only
images remained byte-identical. Writable tests passed the actual nano save,
remount persistence, locking, handle lifetimes, permissions, revision 1 refusal,
read-only preservation and clean unmount. The tmpfs release-build sample was
103.19 MiB/s for a 1 MiB write; this does not replace the USB measurement.

The documented `install -d` / `install -m 755` loop installed all six tools into
a fresh temporary prefix, where each executable matched its built source.
Installed mkfs/check successfully created and checked a 16 MiB image. The
specified removal loop emptied `bin` and preserved that image. No system mount
helpers were registered.

A subsequent review moved the new device remount-persistence assertion into the
unprivileged workload process, because FUSE access belongs to the mounting user.
Python compilation passed after that correction. The final candidate is
repackaged from the clean record/correction commit; its exact hash and source
commit are in the archive sidecars. The privileged gate remains pending and no
historical hardware evidence was modified.

## User-run disposable validation and harness correction

The user ran `python3 scripts/device-validation.py --bin-dir "$PWD/target/release"`.
All three backend tests passed (capacity change, exclusion, mounted-child refusal).
The backend mounted-child setup printed an sfdisk reread warning before its
existing partx fallback; that is distinct from the formatter refresh test.
The corrected formatter test passed both sector sizes, partition refresh and
writable reopen. This resolves the historical formatter ioctl verification gap.

The device FUSE suite then failed readiness detection: root's `mountpoint` probe
could not stat the FUSE mount owned by the dropped user. It timed out and also
missed the mounted filesystem during cleanup, resulting in a secondary temporary
directory removal error. This run is a failure, not a FUSE pass.
The leftover disposable mount was detached with `fusermount3 -uz` as the ordinary
user; no sudo or physical-device operation was performed by the agent.

Readiness and cleanup now inspect `/proc/self/mountinfo` without statting the
FUSE mount. Two regression tests cover owner-only access, exact mountpoint
matching, removal, and mountinfo path escaping; all 21 host Python tests pass.
Only the device FUSE gate now needs the focused user-run command:

```sh
sudo python3 scripts/device-mount-smoke.py --bin-dir "$PWD/target/release"
```

The focused rerun progressed through a successful post-unmount check, then a
later post-unmount checker failed with `Error: LockContention`. This is retained
as a second failed run. The harness now retries only that exact pre-open error
for up to 15 seconds at 100 ms intervals; every attempt passes the loop device's
captured `--expect-disk-sequence`. Other errors and writable operations are not
retried. Three new tests cover transient contention, timeout, unchanged identity
arguments and immediate permanent-error failure. All 24 host Python tests pass.
The device FUSE result remains pending a subsequent rerun.


## Final disposable-device result

The user's final focused run passed both **512-byte and 4096-byte** loop devices.
For each, all three post-unmount checks succeeded. The initial empty filesystem
had 7,640 free blocks and 511 free inodes; after the writable workload and its
read-only remount it retained 7,638 free blocks and 510 free inodes, with matching
saved contents. Ownership, dropped UID/GID and supplementary groups, exclusive
claims, writable operations, retained-file persistence, read-only whole-image
hashes, normal service exits, unmounts and loop detachment all passed.

Together with the preceding backend and formatter passes, this closes the
privileged release gate. The two intermediate FUSE failures above remain part
of the record; neither was counted as a pass. All required validation is now
complete. Cargo metadata also confirms every crate inherits MIT, software 0.0.1,
Rust 1.89 and `publish = false`. No USB trials were repeated, no existing backups
or repository mountpoint files were changed, and no tag or publication was made.

The final clean-HEAD archive is regenerated after this record is committed. Its
SHA-256 and exact source commit are recorded beside it, avoiding a self-referential
commit hash inside this document. Fresh extraction/build and the two image smoke
logs accompany the archive as external verification evidence.
