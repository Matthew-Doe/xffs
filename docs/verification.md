# Writable POC verification

For the completed 0.0.1 source-release checks, see the
[release verification record](release-verification.md). The historical image
acceptance record below is retained. The [COW update](#file-data-cow-verification--2026-09-30)
records the current file-data guarantee and checks.

This verification record covers the temporary-image acceptance run on Linux on
2026-09-28. Physical-device implementation and pending USB acceptance are tracked
separately in the [device results](hardware-results/2026-09-28.md). USB backups
are outside both workflows.
Software remains unreleased 0.0.1, with experimental disk revisions 1 and 2.

## Reproducible checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo run -p xffs-sim --example crash_demo --locked
./scripts/mount-smoke.sh
./scripts/writable-smoke.sh
```

Formatting, Clippy with warnings denied, workspace tests, and the retained crash
demonstration pass with both Rust 1.89.0 and Rust 1.98.1. The Rust 1.89 toolchain
was installed separately under `/tmp`; the project's package versions, declared
minimum, dependency lockfile, and system toolchain were not changed by validation.

## Simulator and adapter coverage

| Area | Verification |
| --- | --- |
| Revisions | Independent revision 1/2 golden bytes; reserved fields, cleanup bounds, timestamps, free slots, exhausted generations, mixed revisions, and revision 1 writable refusal |
| Publication | Injected failures at every write/flush in a transaction; torn prefixes at header, checksum, sequence, and state boundaries; acknowledged changes survive reopening |
| Recovery | Interrupted normalization, checkpointing, retirement, and opening-time reclamation; invalid recovered ownership and missing superblock redundancy cause no writes |
| Allocation | Sparse offsets above 4 GiB, overflow chains, partial writes, full image/inode table, reuse, zero-fill on reused storage, shrink/extend, and memory exhaustion |
| Journal bound | A valid 43,000-extent sparse-backed volume rejects an insertion that needs more than 256 images before any write/flush; a tail append still succeeds |
| Reclamation | Multi-transaction truncation, orphan files, detached empty directories, final close, and crashes during subsequent opening; recovered ownership has no leaks or duplicate claims |
| Namespace | Portable/Unicode conflicts, cycles, nonempty directories, cross-directory/case-only rename, replacement, no-replace, stale generations, and open-unlinked access |
| Adapter | Access flags, append serialization, truncating opens, generation-specific node numbers, stable bounded directory snapshots, modes, backend failure propagation, and disposal of faulted handles |
| Read-only | Spies reject every backend write/flush; retained malformed/recovery fixtures continue to pass |

A failed in-place overwrite is deliberately allowed to leave a mixture of old
and new data. A dedicated torn-write test verifies that this weaker data contract
still leaves metadata recoverable. The simulator models the backend durability
contract; it does not certify a particular physical device's flush behavior.

## Real FUSE acceptance

Both explicit mount scripts pass outside the agent sandbox, where `/dev/fuse`
is available. The read-only script mounts clean and committed images repeatedly,
checks reads and write rejection, and compares whole images byte for byte.

The writable script exercises `mkdir`, `touch`, shell writes/appends, `cat`, `cp`,
`mv`, `rm`, `rmdir`, truncation, and an actual `nano` process controlled through a
pseudo-terminal. It checks multiple handles, immediate read coherence, open-unlinked
reads/writes and zero link count, atomic replacement retaining old handles,
executable flags, explicit timestamps, rejected mode changes, exclusive locking,
and `--noexec`. After unmount it runs `xffs-check` and `xffs-inspect`, remounts,
and compares contents, names, free blocks, and free inodes. Removing the files
restores initial free-space accounting. Revision 1 writable rejection and
read-only mounts leave their images unchanged.

Scripts have overall and subprocess timeouts plus unmount/process cleanup.
Missing prerequisites return **77 / UNMET PREREQUISITE**, never a passing result.
The writable script additionally requires `nano` and a working pseudo-terminal.

## Performance sample and boundaries

A debug-build writable mount wrote 1 MiB in approximately 0.044 seconds
(22.9 MiB/s), including the core's per-block durability barriers. The temporary
image was hosted on **tmpfs**. This is a functional performance sample, not a disk
or USB throughput measurement, and there is no numerical acceptance threshold.

Successful mutating core operations are durable under the storage contract;
application buffering is outside that guarantee. No stable format compatibility,
migration, hard links, symlinks, special files, ACLs,
extended attributes, full Unix ownership/modes, or advanced allocation operations
are claimed. The adapter's direct I/O and serialized transactions favor a simple,
testable POC over performance optimization.


## File-data COW verification — 2026-09-30

This update supersedes the historical mixed-data overwrite expectation above.
The old torn-overwrite test now requires unchanged old contents when the first
fresh-block write tears. The raw simulator demonstration still permits torn
writes; the filesystem prevents those bytes from becoming reachable.

Verified on Rust 1.98.1:

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-targets --locked` | Passed; all 18 writable tests passed; four privileged loop-device tests remain ignored |
| `cargo run -p xffs-sim --example crash_demo --locked` | Passed |
| `./scripts/mount-smoke.sh` | Exit 77: UNMET PREREQUISITE, `/dev/fuse` unavailable |
| `./scripts/writable-smoke.sh` | Exit 77: UNMET PREREQUISITE, `/dev/fuse` unavailable |

The COW matrix derives write/flush failure positions from successful traces.
It covers full and partial overwrite, unaligned multi-block committed prefixes,
same-block and separate-block EOF-tail updates, and extension after shrink.
Each boundary is tested with no persistence and torn prefixes of 1, 80, 2048,
4095, and 4096 bytes. A no-failure run requires successful writes to survive.
Recovery must expose exactly an allowed committed prefix; repeated reopening
preserves contents and allocation counts. Subsequent writes and deletion restore
the original free-space/inode counts. Separate tests interrupt recovery of a
committed COW transaction and verify its replacement data survives.

Full-image overwrites return ENOSPC with no write/flush. With one spare block,
a replacement needing an overflow metadata block and a two-block EOF update both
fail preflight without changing data or free space. Tests retain the existing
256-image transaction and memory limits and add a 65,536-extent limit case;
the writer remains usable after rejection. Mapping tests cover beginning,
middle, end, and coalescing across neighboring extents.

No physical device or destructive test was run, no new performance claim is
made, and the simulator does not certify hardware flush behavior. Earlier
toolchain and real-FUSE results above are historical, not results of this run.

### FUSE retry — 2026-09-30

After sandbox restrictions were removed, both image-backed smoke scripts passed
with exit 0. `./scripts/mount-smoke.sh` verified clean and recovered read-only
mounts and confirmed complete images remained unchanged.
`./scripts/writable-smoke.sh` passed terminal operations, nano, durability,
open-handle lifetimes, permissions, locking, and remount checks. The earlier
exit-77 results above record the initial restricted environment; the FUSE
prerequisite is now satisfied for this run. No physical-device test was run.
