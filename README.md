# XFFS

Experimental filesystem POC for regular image files, with durable core mutations,
metadata redo journaling, writable recovery, and a deterministic crash simulator.
Linux FUSE mounts are read-only by default; `--rw` enables everyday file and
directory operations, including editor saves and atomic rename/replacement.

Software remains **unreleased 0.0.1**. New images use **experimental format revision
2**. Revision 1 images remain readable but cannot be written or automatically
converted. These identifiers describe disk encodings, not software releases.
There is no promise of stable disk compatibility.

- `xffs-core`: storage API, checked codecs, Unicode names, recovered reader/writer.
- `xffs-sim`: deterministic crash/fault model and the retained crash demonstration.
- `xffs-tools`: formatter, demo generator, raw inspector, recovered checker.
- `xffs-fuse`: foreground Linux FUSE adapter using fuser without libfuse.

All crates are unpublished, prohibit unsafe code, and require Rust 1.89 or newer.
Dependencies are recorded in Cargo.lock; Unicode/CRC/FUSE versions are pinned.
No project license has been selected; release packaging is deferred.

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

Omit `--rw` for a shared-lock read-only mount. Writable mounts hold an exclusive
image lock. Both modes retain `nosuid,nodev,default_permissions`; `--noexec`
prevents execution. Writable ownership is the mounting user's; directories and
executable files use 0755, ordinary files 0644. Chmod changes execute bits only.
Names preserve spelling and use Unicode 16.0 canonical-caseless matching.

Successful mutating core operations are durable under the
[storage contract](docs/storage-contract.md). Application buffering is outside
that guarantee. Interrupted in-place overwrites can contain mixed old/new data.
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

Physical devices, USB access, migration, importers, hard links, symlinks, special
files, full Unix ownership/modes, ACLs, xattrs, and advanced allocation operations
remain deferred. Development and acceptance tests operate only on image files.

See the [format specification](docs/on-disk-format.md),
[writable API and mount behavior](docs/writable.md),
[image tools and fixtures](docs/image-tools.md),
[read-only validation](docs/read-only.md), and [codecs](docs/codecs.md).
Historical design records remain in [design-decisions.md](design-decisions.md),
[filesystem-design-and-development-plan.md](filesystem-design-and-development-plan.md),
and [filesystem-spec-pain-points-and-0.0.1.md](filesystem-spec-pain-points-and-0.0.1.md).

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
