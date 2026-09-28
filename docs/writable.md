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
