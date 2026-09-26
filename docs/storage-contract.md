# Implemented storage contract (phases 0–1)

This document specifies the current API, not an on-disk filesystem format.
Design choices remain mutable. Changes to established durability semantics must
be explicit before later binary-format and transaction work relies on them.

## Common byte-addressed interface

`xffs_core::BlockDevice` is synchronous:

```rust
pub trait BlockDevice {
    fn capacity_bytes(&self) -> u64;
    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> Result<(), DeviceError>;
    fn write_at(&mut self, offset: u64, source: &[u8]) -> Result<(), DeviceError>;
    fn flush(&mut self) -> Result<(), DeviceError>;
}
```

Capacity is fixed for a handle. Ranges are half-open and may be unaligned.
`offset + length` must be representable as `u64` and at most capacity. Bounds are
checked before I/O. Invalid reads leave the destination unchanged, and invalid
writes leave storage unchanged. Empty requests succeed at offsets from zero
through capacity, except that **all writes on a read-only handle fail**, including
empty writes. Offsets beyond capacity fail even for empty requests. Range errors
take precedence over read-only rejection. Simulator resource exhaustion can
reject even an empty request before it executes.

Successful reads and writes transfer the entire request. A successful write is
visible to subsequent reads through the handle; it does not establish durability.
An error may follow a transferred prefix. For reads only that prefix of the
caller's destination changes; for writes that prefix may be visible or durable.
Never treat a partial transfer as success or an error as rollback.

Successful writable flush establishes durability for all preceding writes,
including transferred prefixes of failed writes. A later crash can change those
bytes only through subsequent writes selected for persistence. There is no
sector or block atomicity assumption. Read-only flush is a successful no-op.
Drop does not promise a flush. Filesystem-level recovery and decisions to stop
mutation belong to the future transaction engine, not the backend.

## Errors

`DeviceError` implements `Display` and `std::error::Error` and distinguishes:

| Variant | Meaning |
| --- | --- |
| `InvalidRange` | Offset, length, and fixed capacity describe an invalid/overflowing range. |
| `ReadOnly` | Valid write request denied by the handle's access mode. |
| `LockContention` | Nonblocking lock would block. |
| `UnsupportedFileType` | The image path/handle is not a regular file. |
| `Io` | Underlying error, operation, optional starting offset, and optional known transferred byte count. The source error is retained. |
| `InjectedFault` | Simulated read/write/flush failure with operation, starting offset where applicable, and transferred count. |
| `InvalidScenario` | Invalid capacity, fault prefix, stale write ID, or fragment range. |
| `ResourceLimit` | Named simulator resource and its limit. |

For image read/write failures, offsets refer to the original request and the
transferred count is always known (zero on seek failure). Open, metadata, lock,
and flush errors have no byte offset or transfer count. Unexpected EOF and
zero-progress writes are `Io` errors with `UnexpectedEof` and `WriteZero` sources.
Injected flush failures report zero transferred bytes; their trace selections,
not this count, describe persistence.

## Image backend

`ImageDevice::open(path, AccessMode)` opens **existing regular files only**,
without creation, truncation, or resizing. It rejects known special files before
opening and checks the opened handle again. Symlinks to regular files are
accepted. The caller must supply a stable, trusted path: concurrent replacement
of path components is unsupported, as is external file modification while open.
This portable standard-library API is not a secure opener for adversarial paths.
Zero-length regular images are accepted, with only empty operations in bounds.

Read/write handles hold a nonblocking exclusive file lock. Read-only handles
hold a shared lock. Contention is a typed error; other locking failures are I/O
errors. Locks live until handle drop, and cooperating readers may coexist.
They do not protect against programs that ignore the platform's locks. Capacity
is captured under the lock. The backend owns its file and does not expose clones
or unlock operations.

The backend seeks, retries interrupted operations, and loops over short reads or
writes. Flush uses `File::sync_all` for writable handles, with interrupted calls
retried. Its durability guarantee is conditional on the host filesystem, OS,
and device honoring synchronization. Reopening an image is not an experiment
that cuts power to the host. See the standard-library
[`File` locking and synchronization reference](https://doc.rust-lang.org/std/fs/struct.File.html).

## Deterministic simulator

`SimDevice::new(capacity)` starts with zeroed durable and visible bytes and
read/write access. `with_access` also permits read-only access; `Default` selects
1 MiB. Inspection accessors expose immutable slices of durable bytes, visible
bytes, pending writes, and trace events.

Reads use visible bytes. Nonempty writes retain the transferred payload and
update visible bytes in issue order. Each accepted write with a nonzero prefix
has a unique write ID equal to its trace operation ID. Empty writes and failures
before transfer create no pending record. Successful writable flush copies the
entire visible image to durable storage and retires all pending records. Normal
writes do not persist implicitly; scripts explicitly choose persistence at a
failed flush or crash boundary.

### Fault controls

- `fail_next_read(n)` and `fail_next_write(n)` arm/replace a one-shot fault after
  exactly `n` bytes of the next valid nonempty request. Zero means before transfer;
  request length means a full transfer followed by an error. A prefix beyond the
  request is an invalid scenario and does not transfer or consume the fault.
- Invalid ranges, read-only writes, empty requests, and resource rejection do
  not consume an armed transfer fault. A partial write retains only its prefix;
  persistence plans cannot refer to the untransferred suffix.
- `fail_next_flush(fragments)` validates and arms/replaces a failed writable
  flush. The flush revalidates its selection, applies it to durable bytes, and
  returns an injected error. Visible bytes and all pending records remain intact.
  An empty selection persists nothing. A subsequent successful flush durably
  installs visible bytes and clears pending records. Read-only flush ignores
  armed faults.
- `clear_faults()` disarms all faults. Configuration calls are not device
  operations and are not traced; the resulting operation events describe effects.

### Fragment persistence and restart

A `Fragment` is a pending write ID and a half-open `Range<usize>` within its
retained payload. `crash_and_restart(&fragments)` validates the **entire** list,
then applies its fragments to durable bytes in list order. Last-applied bytes
win where ranges overlap. It drops all pending records, copies durable bytes
back to visible bytes, and clears armed faults. The write/operation ID sequence
continues across restarts and trace clearing.

An empty plan drops all pending writes. Arbitrary subranges model torn writes;
omitting IDs models selective persistence; ordered fragments model reordering.
Fragments may repeat or overlap, and empty ranges within a valid payload are
allowed. Unknown/flushed/restarted IDs, reversed ranges, out-of-payload ranges,
and oversized plans fail before any mutation. Rejected crash plans and rejected
flush-fault configurations leave **all** state, including trace and IDs, unchanged.

A failed flush does not retire pending writes: a later crash plan may select
those same fragments again, including overwriting bytes persisted by the failed
flush. This is an explicit replay/persistence choice, not automatic replay.
Already durable bytes remain untouched wherever the later plan writes nothing.
Successfully flushed records cannot be selected again because their IDs are
stale. Subsequent writes can of course overwrite previously flushed locations.

### Trace and resource limits

Each attempted read, write, or flush records one `TraceEvent` with a monotonic
operation ID, request range where applicable, and completion/failure with known
transferred count. Flush events identify a normal flush or its fault selections;
successful crash events include the ordered selections. Failure reason strings
are diagnostic, not a stable machine error API; use `DeviceError` for that.
Identical calls and inputs yield identical traces and durable bytes. No random
source, timestamps, file paths, or host-generated IDs enter the simulation.

| Resource | Bound |
| --- | --- |
| Capacity | Nonzero; default 1 MiB; maximum 64 MiB |
| Pending payload bytes | 16 MiB |
| Pending write records | 4,096 |
| Fragments in one persistence plan | 4,096 |
| Retained trace events | 100,000 |
| Operation IDs | Checked `u64` counter; exhaustion is an error |

The fragment-count bound additionally limits plan validation and per-event
selection storage. These are simulator limits, not image or filesystem limits.
Pending byte/record limits apply to the actual transferred prefix and are checked
before accepting any of the triggering write. Rejection is traced if space is
available; data, pending records, and armed faults remain unchanged.

Trace exhaustion fails before the operation changes state or consumes an ID or
fault; the rejected operation itself cannot be recorded. There is no silent
truncation. Call `clear_trace()` explicitly between scenarios or after exhaustion.
It clears only retained events, keeping IDs, data, pending writes, and faults.
Host allocation failure is subject to Rust's allocation behavior; the logical
limits do not guarantee successful allocation under host memory pressure.

## Verification boundary

The shared contract exercises bounds, overflow, empty requests, unaligned and
overlapping I/O, access modes, visibility, and flush on both backends. Controlled
I/O doubles exercise interruption, short transfers, EOF, zero progress, and
partial errors. Temporary-image tests verify flush/reopen and shared/exclusive
locking, including contention from a separate process.

Simulator regressions cover loss, durability, tearing, selective/reordered
persistence, partial faults, failed flushes, invalid plans without mutation,
resource exhaustion, deterministic traces, and repeated restarts. The runnable
`crash_demo` asserts all four milestone outcomes. These tests establish the
scripted device model; they make no claim of exhaustive real-hardware behavior.
