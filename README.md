# XFFS

XFFS is an experimental filesystem intended as an alternative to FAT32 and
exFAT for USB flash drives and other removable storage. Its goal is straightforward
file storage with recoverable metadata and explicit durability guarantees when a
drive is disconnected unexpectedly.

The current implementation runs on Linux through FUSE and supports whole-disk
flash drives as well as image files for development and testing. It is an early
proof of concept: it has no native Windows or macOS driver, no stable on-disk
compatibility promise, and is not yet a general-purpose replacement for FAT32 or
exFAT. Measured USB write performance and durability limits are documented below.

XFFS provides metadata redo journaling, writable recovery, and a deterministic
crash simulator. Mounts are read-only by default; `--rw` enables everyday file and
directory operations, including editor saves and atomic rename/replacement.

Software version is **0.0.1**, an experimental source release. New images use **experimental format revision
2**. Revision 1 images remain readable but cannot be written or automatically
converted. These identifiers describe disk encodings, not software releases.
There is no promise of stable disk compatibility.

- `xffs-core`: storage API, checked codecs, Unicode names, recovered reader/writer.
- `xffs-sim`: deterministic crash/fault model and the retained crash demonstration.
- `xffs-tools`: formatter, demo generator, raw inspector, recovered checker.
- `xffs-fuse`: foreground Linux FUSE adapter using fuser without libfuse.

All crates are unpublished, prohibit unsafe code, and require Rust 1.89 or newer.
Dependencies are recorded in Cargo.lock; Unicode/CRC/FUSE versions are pinned.
Licensed under [MIT](LICENSE), copyright 2026 Matthew Doe. See the
[0.0.1 source release instructions](docs/release-0.0.1.md) and
[release verification record](docs/release-verification.md). Releases are available
on [GitHub](https://github.com/Matthew-Doe/xffs/releases).

```sh
cargo build --workspace --locked
# Refuses to overwrite an existing path; choose a UUID for your image.
target/debug/mkfs-xffs disk.img --size-mib 64 --uuid 58464653-0000-0001-8000-000000000001
mkdir -p mnt
target/debug/mount-xffs disk.img mnt --rw
# In another terminal:
mkdir mnt/notes
printf 'Hello\n' > mnt/notes/readme.txt
nano mnt/notes/readme.txt
cat mnt/notes/readme.txt
fusermount3 -u mnt
target/debug/xffs-check disk.img
# Remount with the same command to verify saved contents.
```

Omit `--rw` for a read-only mount. Read-only images hold shared locks; writable
images and all physical-device handles hold exclusive locks. Both modes retain `nosuid,nodev,default_permissions`; `--noexec`
prevents execution. Writable ownership is the mounting user's; directories and
executable files use 0755, ordinary files 0644. Chmod changes execute bits only.
Names preserve spelling and use Unicode 16.0 canonical-caseless matching.

Successful mutating core operations are durable under the
[storage contract](docs/storage-contract.md). Application buffering is outside
that guarantee. File-data updates use fresh blocks and recover as old or complete
replacement contents per 4-KiB transaction. Overwrites require spare space and
can return ENOSPC; multi-block requests can complete only a prefix.
For atomic whole-file replacement, write and fsync a temporary file, rename it
over the destination, and fsync its parent directory. Open handles retain the
old file until closed.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --locked
cargo run -p xffs-sim --example crash_demo
# Explicit real FUSE tests; require /dev/fuse and fusermount3:
./scripts/mount-smoke.sh
./scripts/writable-smoke.sh  # also requires nano
```

Ordinary tests do not require FUSE. Missing mount prerequisites produce exit 77
and **UNMET PREREQUISITE**, never a pass. Mount tests use temporary images,
timeouts, and unmount cleanup. See the [verification record](docs/verification.md)
for the crash matrix, toolchain checks, real mount results, and performance sample.

For deterministic recovery fixtures, use `xffs-image create-demo IMAGE --scenario
committed`. Both creators accept `--format-revision 1|2` (default 2).
Cargo target names are `mount-xffs` and `mkfs-xffs`; `scripts/mount.xffs` and
`scripts/mkfs.xffs` provide conventional dotted wrappers.

Migration, importers, hard links, symlinks, special files, full Unix ownership/modes,
ACLs, xattrs, and advanced allocation operations remain deferred. Ordinary tests
use images and simulated devices; privileged loop and USB tests require explicit
invocation. USB workload, reconnect, and removal trials have passed; remaining reporting
and verification limits are recorded below.

See the [format specification](docs/on-disk-format.md),
[writable API and mount behavior](docs/writable.md),
[image tools and fixtures](docs/image-tools.md),
[read-only validation](docs/read-only.md), and [codecs](docs/codecs.md).

### Explicit Linux whole-disk access

Image commands remain the default. Physical devices require `--device`; only
whole disks with 512-byte or 4096-byte logical sectors are supported. The format
is still experimental revision 2, software version 0.0.1.

```sh
sudo target/debug/mkfs-xffs /dev/disk/by-id/YOUR-USB --device --erase \
  --expect-serial YOUR-SERIAL --uuid 58464653-0000-0001-8000-000000000001 --inodes 65536
sudo target/debug/xffs-check /dev/disk/by-id/YOUR-USB --device --memory-mib 512
sudo target/debug/xffs-inspect /dev/disk/by-id/YOUR-USB --device
```

Formatting uses the detected capacity; `--size-mib` is image-only. Wrong serial,
unsupported geometry, invalid layout, active mounts, swap or holders are refused
before writing. Both old superblocks are invalidated and flushed before metadata
changes. Metadata and conventional MBR/GPT regions are initialized, flushed,
and only then are new superblocks published and flushed separately. This is not
secure erasure of all previous contents. Interrupted formatting is reported and
never rolled back. No physical device is truncated, resized, removed or restored.
After releasing its claim, mkfs invokes util-linux `blockdev --rereadpt`, checks
identity again, and verifies that no kernel partitions remain. Refresh failure
is an error even when the on-disk format completed.

Empty image creation uses the same streaming formatter. Revision 1 image
fixtures remain available. Images are still created exclusively, and only newly
created images are removed on creation failure.

Mount a physical device in an isolated mountpoint owned by your user:

```sh
mkdir -p /tmp/xffs-usb-mount
sudo target/debug/mount-xffs /dev/disk/by-id/YOUR-USB /tmp/xffs-usb-mount \
  --device --rw --memory-mib 512
# In another terminal, edit files, then cleanly unmount:
fusermount3 -u /tmp/xffs-usb-mount
```

Omit `--rw` for read-only service; `--noexec` remains available. Root claims the
device first, drops supplementary groups and UID/GID to the sudo invoking user,
and only then performs recovery and starts FUSE. Root without an identifiable
invoking user must supply non-root `--uid UID --gid GID`. The mountpoint must be
accessible to that user. The device claim lasts until service exits. On removal,
I/O fails and a writable service faults; reconnect requires a fresh mount.
`--memory-mib` uses the same checked conversion as xffs-check. Image mounts retain
their previous ownership and access behavior.

Disposable real device/FUSE verification (explicit, root required):
`cargo build --workspace --locked` followed by
`sudo python3 scripts/device-mount-smoke.py`. It creates its own loop images and
temporary mountpoints, checks both logical sector sizes, ownership and dropped
privileges, exclusive access, read-only byte preservation, writable operations,
and clean remounts. Missing prerequisites exit 77, not a pass. Physical removal
requires the separate hardware trials.

The serial-bound destructive USB harness, manual unplug protocol, and evidence
format are documented in [USB acceptance](docs/usb-acceptance.md). See the
[latest verification report](docs/hardware-results/2026-09-29.md) for completed hardware acceptance and measured USB performance. The observed
8 MiB write including fsync averaged 0.137 MiB/s; the buffered read after
reconnect averaged 205.9 MiB/s. These measure this implementation, not raw media
bandwidth. Temporary image results are not USB acceptance.

For reproducible XFFS/exFAT/FAT32/ext4 comparisons, see
[filesystem benchmarking](docs/benchmarking.md). The separate benchmark harness
supports disposable loop images, explicitly authorized whole-USB-disk runs,
and offline report regeneration from saved results.
