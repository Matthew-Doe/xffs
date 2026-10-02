# XFFS 0.0.2 COW source-release candidate

This candidate adds file-data copy-on-write while retaining the metadata redo
journal. It is prepared for a separate tagging/publishing decision; this document
does not claim that a 0.0.2 tag or public release exists.

Every file-data update uses fresh storage, including partial overwrites and
EOF-tail zeroing. Under the [storage contract](storage-contract.md), recovery
exposes previous or complete replacement contents per block transaction.
Successful mutations are durable; a multi-block request may return a short
write, and an I/O error may be reported after commit.

Overwrites require spare space and may return ENOSPC. Extent fragmentation can
need additional metadata storage or reach existing extent/transaction limits.
Preflight failures do not mutate the current transaction and leave the writer
usable. There is no in-place fallback.

On-disk revision **2 is unchanged**: existing revision 2 images need no migration.
Older writers can still open them but do not provide this COW guarantee.
Revision 1 stays read-only. APIs, CLI options and request limits are unchanged.
Snapshots, reflinks, metadata COW, batching and whole-request atomicity are
excluded. For whole-file atomic publication, write and fsync a temporary file,
rename it over the destination, then fsync the parent directory.

## Verification and installation

See [candidate verification](release-verification-0.0.2.md) and the
[physical COW evidence](hardware-results/2026-10-01.md).
The physical interruption occurred during journal retirement after data commit;
it does not replace exhaustive simulator testing.

The supported build prerequisites remain Linux, Rust 1.89+, Python 3.11+ for
harnesses, FUSE/fusermount3 and nano for image acceptance. Build with:

```sh
sha256sum -c xffs-0.0.2.tar.gz.sha256
tar -xzf xffs-0.0.2.tar.gz
cd xffs-0.0.2
cargo build --release --workspace --locked
./scripts/mount-smoke.sh --bin-dir "$PWD/target/release"
./scripts/writable-smoke.sh --bin-dir "$PWD/target/release"
```

The [0.0.1 installation instructions](release-0.0.1.md#build-and-install-from-source)
also apply to these six executables; use the 0.0.2 archive name. The source
packager now derives its archive name from the workspace version:

```sh
python3 scripts/package-source.py --output-dir /tmp/xffs-cow-release
```

It requires a clean tracked HEAD, includes tracked sources and Cargo.lock,
excludes untracked hardware evidence, and writes SHA-256 and source-commit
sidecars. It does not tag, upload, format hardware, or register system helpers.
