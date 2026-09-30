# Source release verification — 2026-09-29

Status: **pending**. No tag or publication has been made. The historical USB
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

## Privileged gate

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
