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
