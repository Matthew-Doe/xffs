# XFFS 0.0.1 experimental source release candidate

Release status: **validated source candidate; not tagged or published**. Licensed under
[MIT](../LICENSE), copyright 2026 Matthew Doe. Crates retain `publish = false`.

New filesystems use experimental format revision 2. Revision 1 is read-only;
there is no automatic conversion or stable future disk compatibility promise.
This release includes image and exclusive Linux whole-disk access, streaming
formatting, metadata redo recovery, Unicode names, a writable FUSE adapter and
crash simulation. Post-format claims retry typed contention every 100 ms for up
to 15 seconds per claim and compare the full original identity, including disk
sequence. Refresh or verification failure remains fatal after writing; never
repeat a format to resolve that failure.

Durability depends on the backend honoring writes and flushes as described in
[the storage contract](storage-contract.md). Application buffering is outside
that guarantee. Interrupted in-place overwrites may mix old and new data; use
fsynced temporary files and rename for whole-file replacement. The simulator does
not certify physical media. Migration, importers, links, special files, ACLs,
xattrs, full Unix ownership/modes and advanced allocation remain deferred.

The completed USB trials are preserved in [the hardware report](hardware-results/2026-09-29.md).
The measured 8 MiB write including fsync took 58.502580 s (0.136746 MiB/s);
the buffered read after reconnect took 0.038858 s (205.875459 MiB/s). These are
FUSE/backend observations, not raw media bandwidth or guaranteed cold-cache reads.
No physical drive reformatting or additional unplug trials are required here.

## Build and install from source

Requirements: Linux, Rust/Cargo **1.89 or newer**, a C linker/toolchain, and access
to the locked Cargo dependencies (or a populated Cargo cache). FUSE operation
requires `/dev/fuse`, FUSE kernel support and `fusermount3` from FUSE 3 utilities;
libfuse development headers are not required. Whole-disk formatting additionally
uses util-linux `blockdev`. Disposable validation uses `losetup`, `udevadm`,
`timeout`, Python 3.11+ and root; image editor validation requires `nano` and a PTY.

```sh
sha256sum -c xffs-0.0.1.tar.gz.sha256
tar -xzf xffs-0.0.1.tar.gz
cd xffs-0.0.1
cargo build --release --workspace --locked
prefix="$HOME/.local"
install -d "$prefix/bin"
for tool in mkfs-xffs mount-xffs xffs-check xffs-image xffs-inspect xffs-verify-format; do
    install -m 755 "target/release/$tool" "$prefix/bin/$tool"
done
```

Add the selected prefix's `bin` to PATH. Invoke `mkfs-xffs` and `mount-xffs`
directly, following the image examples in the README. These instructions do not
register system mount helpers. Development wrappers in `scripts/` remain available.
Do not overwrite another installation when testing removal; use a fresh prefix.
Remove this installation with:

```sh
for tool in mkfs-xffs mount-xffs xffs-check xffs-image xffs-inspect xffs-verify-format; do
    rm -- "$prefix/bin/$tool"
done
```

Unmount filesystems before removing tools. Removal leaves images and data intact.

## Source packaging and release gate

From a Git checkout with a clean tracked tree and index:

```sh
python3 scripts/package-source.py --output-dir /tmp/xffs-release
```

This archives tracked HEAD files, including Cargo.lock, LICENSE, documentation,
tests and scripts; untracked hardware logs and build outputs are excluded. The
SHA-256 sidecar verifies transport integrity, not publisher authenticity. The
`.source-commit` sidecar and tar pax comment identify the exact source commit.
No tag or upload is made. Extract into a fresh directory and build as above, then:

```sh
./scripts/mount-smoke.sh --bin-dir "$PWD/target/release"
./scripts/writable-smoke.sh --bin-dir "$PWD/target/release"
```

The writable test drives a real nano save. To complete privileged disposable
validation, run the following yourself as your ordinary user from the checkout
(or extracted source), with the release executable directory selected:

```sh
python3 scripts/device-validation.py --bin-dir "$PWD/target/release"
```

That explicitly invoked harness uses sudo for backend and formatter loop tests
and device-backed FUSE. It never selects a physical disk. Both 512-byte and
4096-byte loop geometries are required, including removal of an old partition
table's kernel children. Ownership, privilege drop, exclusion, writable
persistence, read-only byte preservation and teardown must all pass.
Missing prerequisites (including exit 77) leave the release pending.
See [the current verification record](release-verification.md) before tagging.
