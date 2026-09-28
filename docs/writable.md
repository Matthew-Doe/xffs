# Writable image implementation

`ReadWriteFs::open` acquires an exclusive image-file lock. The device constructor
requires its caller to provide exclusive access. Revision 1 is rejected without
writes; no migration is performed. Both superblocks must be valid and agree.
The shared read-only validator checks the complete recovered view before recovery
performs any writes. Recovery first normalizes both journal controls, then
checkpoints committed images and retires both controls. Interrupted normalization,
checkpointing, and retirement can be repeated on the next open.

Transactions preflight target types, bounded memory, 256-image capacity, and two
sequence increments. They initialize data and flush; write payload and flush;
write and flush each committed control; checkpoint and flush; then write and
flush each clean control. Only then is the in-memory result published. Counters
never wrap. Journal payload is never reused before both controls are retired.

A backend error after mutation starts faults the writer. Subsequent operations
return an I/O-class error until reopening. Successful mutating core calls are
durable under the storage contract. Application buffering is outside that
contract. In-place data overwrites are not atomic: interrupted writes can leave
mixed old and new data while metadata remains recoverable.

Block allocation uses deterministic next-fit search and never reuses storage
being freed by the same transaction. Adjacent logical/physical mappings coalesce.
New data blocks and exposed tails are initialized before metadata publication;
sparse gaps remain unmapped and read as zeros. Each write request is at most 1 MiB
and progresses in block-sized durable transactions. A short write reports only
completed transactions, including when a later backend failure faults the writer.

Shrinking publishes the new size and old-size cleanup bound atomically. Cleanup
releases at most 64 trailing data blocks per transaction, updating mappings,
overflow chains, and bitmap together. Opening a writer resumes cleanup and
reclaims detached inodes before returning. Extension zeros the former EOF tail
before increasing size, so shrink/extend cannot reveal old contents.

The allocation bitmap is retained as an ownership index after full opening
validation. Mutations stage only affected inodes, mappings, bitmap blocks, and
metadata images; they do not rescan the volume. Working-memory checks reserve
space for staging and affected cached nodes before mutation. Indivisible edits
larger than 256 metadata images return `TooBig` (`E2BIG` at the adapter).

Namespace operations validate portable names and canonical-caseless uniqueness.
Directory insertion reuses a block's available payload; deletion compacts only
that block and releases trailing empty blocks. Rename publishes both parents,
the moved inode, and any detached replacement in one journal transaction. It
supports case-only changes and no-replace, rejects cycles and nonempty directory
replacement, and never silently converts between file and directory kinds.

Inodes use the lowest available slot and increment its retained generation.
`open_handle`/`close_handle` retain identities across unlink and replacement.
Detached ownership is durable before reclamation; final close reclaims storage.
Cleanup errors after publication are reported without promising rollback.
Closing handles remains allowed on a faulted writer; reopening finishes cleanup.

For atomic whole-file replacement: create a temporary file in the destination
directory, write all contents, fsync the file, rename over the destination, then
fsync the parent directory. Existing open handles retain the previous file.

## Mounting

Build with `cargo build --workspace`, create a revision 2 image with
`target/debug/mkfs-xffs disk.img --size-mib 64 --uuid <UUID>`, then run
`target/debug/mount-xffs disk.img mountpoint --rw`. The process stays in the
foreground. Unmount with `fusermount3 -u mountpoint`, and validate with
`target/debug/xffs-check disk.img`. Omitting `--rw` preserves read-only behavior.
Image files are the only supported backing storage; physical devices are deferred.

Writable mounts use mounting-user ownership, directories/executable files 0755,
and ordinary files 0644. Chmod can change execute bits only; any nonempty execute
bit combination becomes 0755. Other permission changes and nontrivial chown are
rejected. Explicit access/modification timestamps use whole seconds; reads do not
update access time. `--noexec` prevents execution. `nosuid`, `nodev`, and kernel
permission checks remain enabled.

All adapter operations serialize. Append selects EOF while holding that same
lock. Handles record access flags and generation-bearing identities. FUSE node
numbers are never recycled during a mount, even when disk slots are reused.
Writable file opens use direct I/O and writeback caching stays disabled; metadata
TTLs are zero. Directory opens snapshot names, node numbers, and kinds for stable
cookies through concurrent namespace changes. The adapter limits handles to
65,536, snapshots to 16 MiB total, and node mappings to 262,144 per mount.
