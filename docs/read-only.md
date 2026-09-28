# Recovered read-only access

`ReadOnlyFs::open(path)` opens an ImageDevice with ReadOnly access and retains
its shared lock until drop. `open_with_options` accepts a memory limit;
`from_device` supports deterministic test backends with the same stable-capacity
contract. Internally a private facade exposes only capacity/read/block operations.
No reader, checker, recovery or adapter operation calls write_at or flush.

Open validates both superblocks, selects journal controls, verifies every
committed descriptor/image and the complete transaction CRC, and only then
installs an overlay (at most 256 blocks). All metadata reads use that overlay.
Full bitmap, inode-table, extent, directory, generation and ownership checks run
before success. This includes detached orphan/pending regular files, whose
blocks remain allocated but whose identities cannot be accessed through the
public namespace. Reserved allocations, unused bitmap bits, table padding,
equivalent names, disconnected cycles and unexplained allocations are checked.

The reader retains validated inodes/mappings/directory records. getattr and file
reads reread/check the inode table and overflow chain. Lookup and readdir also
reread/check directory blocks against their validated records. Metadata changes
or I/O failures are errors; mapped data failures never become holes. This is not
a guarantee against programs that ignore the shared lock and modify the image.
Raw data is not checksummed by experimental format revision 1.

The public API uses `(index,generation)` identities. `lookup` accepts UTF-8 name
bytes; `getattr` returns the disk inode; `read_dir` accepts a zero-based cookie
and page limit (1..1024), returning entries, next cookie and EOF. Core directory
pages exclude dot entries. `read_file` accepts a byte offset and at most 1 MiB,
stops at EOF, and zero-fills only unmapped ranges. `statfs` counts blocks including
reserved storage and reports fixed inode capacity, free blocks/inodes, 4096-byte
blocks and 255-byte names. Diagnostics report superblock/control fallback and
selected overlays.

## Working-memory limits

The default logical working-memory budget is 128 MiB, configurable through core
OpenOptions and `xffs-check --memory-mib`. The reader charges 12 MiB for bounded
scratch/reply/normalization/chain storage, plus bitmap and ownership bitsets,
overlay storage, and conservative per-inode/extent/name/container charges before
retaining them. Inactive inode slots are streamed rather than allocated. Charges
include temporary tree traversal and comparison-key sets and intentionally are
not reclaimed during opening, so a valid large or dense volume can require a
higher limit. Exhaustion returns ResourceLimit, never partial validation success.
This bounds application working data, not allocator bookkeeping or process RSS;
external memory exhaustion still depends on Rust/the host allocator.

## Linux adapter

`mount-xffs IMAGE MOUNTPOINT [--noexec]` runs in the foreground with one fuser
worker and a mutex serializing all state operations. Root maps to FUSE inode 1;
other indices map to index+1. Lookup returns disk generations; handles retain
and check the complete identity and file/directory kind. At most 65536 handles
are active, IDs monotonically increase without wrapping, and stale/mismatched
handles fail. Dot entries use cookies 1/2; subsequent entries use core position
plus 2, permitting resumable readdir.

UID/GID are the mounting user's IDs. Directories use 0555, files 0444, executable
files 0555; noexec removes file execute bits and adds the kernel noexec option.
Mount options include ro, nosuid, nodev, default_permissions and noatime. Entry
and attribute TTLs are zero, ENOENT replies introduce no negative TTL, and no
writeback capability or keep-cache flags are requested. Fuser's default
capability set omits WRITEBACK_CACHE; the adapter does not add it.

Write-capable opens and mutation callbacks return EROFS. Unsupported optional
operations use fuser's ENOSYS defaults; xattrs return EOPNOTSUPP, and readlink
returns EINVAL because the format has no symlinks. Corruption and backend errors
map to EIO, missing entries to ENOENT, stale IDs to ESTALE and memory exhaustion
to ENOMEM. Read-only flush/fsync validate handles and metadata; release frees a
handle. None calls backend flush or write.

## Verification boundary

Workspace tests use spies that panic on any backend write/flush. They compare
all namespace records, metadata and selected file ranges against an independently
generated clean fixture, including repeated opens, overflow mappings, Unicode,
pagination and sparse reads across 4 GiB. Regressions cover recovery selection,
retirement, invalid transaction checksums, duplicate/forbidden targets, bitmap and
extent overlays over damaged home blocks, fallback/conflicts, ownership, stale
references, detached owners, and resource exhaustion. Adapter tests exercise the
same state methods used by callbacks without requiring /dev/fuse.

Run `scripts/mount-smoke.sh` explicitly for kernel-level validation. It mounts
clean and committed images twice, verifies ls/find/stat/cat, byte patterns,
Unicode lookup, executability/noexec and mutations returning EROFS, then unmounts
and compares entire images against copies. Individual subprocess timeouts, an
overall timeout, and finally-based cleanup bound the test. Exit 77 identifies
missing prerequisites and must not be counted as a passing mount test.

Validated here with Rust 1.98.1: formatting, Clippy with denied warnings,
workspace tests and the crash example. The declared minimum remains 1.89; a
1.89 compiler is not installed in this environment. The real mount smoke test
also passed on 2026-09-27 outside the agent sandbox: clean and committed images
each mounted with execution enabled and with noexec, all read/write-rejection
checks passed, and complete image contents were unchanged. The sandbox hides
/dev/fuse; its missing-prerequisite result describes sandbox visibility, not
the host device.