# XFFS: hobby flash-drive filesystem design and development plan

Status: proposed v0.0.1 design, not an implemented format.

All design decisions are mutable at the user's request. This document records
the current plan, not permanent commitments. Revise it alongside
`design-decisions.md` when choices change; explicitly version incompatible disk
formats and changed guarantees rather than silently reinterpreting old images.

This document supersedes the implementation scope in
`filesystem-spec-pain-points-and-0.0.1.md`. That document remains the original
motivation. XFFS is a working name. No backward compatibility is promised for
experimental images until a format revision is explicitly frozen.

The user explicitly accepts experimental format breaks and reformatting (D-003).
Snapshot-ready architecture and migration tools are not initial requirements.

## 1. What we are building

A small, understandable filesystem for removable, block-addressed flash storage,
with files larger than FAT32 permits, checksummed metadata, explicit crash
recovery, and a portable userspace implementation.

This is a hobby project. Success means an implementation we can inspect, explain,
mount, break deliberately, and recover predictably. It does not require device
vendor adoption, competitive benchmarks, or production certification.

The user's selected primary goal for v0.0.1 is **crash recovery you can understand
and demonstrate**. When priorities conflict, this takes precedence over flash
performance optimization and broad OS interoperability. Individual mechanisms
remain subject to the design interview recorded in `design-decisions.md`.

The first useful demonstration is:

1. Format an image and mount it through Linux FUSE.
2. Create, verify, and copy a file larger than 4 GiB.
3. Interrupt file creation and rename at every simulated write boundary.
4. Recover a structurally valid filesystem with the documented operation result.

### 1.1 Corrected motivation

| Complaint | Accurate baseline | XFFS response |
|---|---|---|
| FAT32 file size limit | FAT32 cannot represent files of 4 GiB or larger; exFAT already supports large files. | 64-bit sizes and explicit checked implementation limits. |
| Weak integrity protection | exFAT has boot-region and directory-entry-set checksums, but not comprehensive metadata or file-content integrity. | Checksums on every metadata block; file-content checksums deferred. |
| Interrupted updates | Ordinary FAT/exFAT do not provide the transaction model proposed here; TexFAT is a separate extension. | Recoverable metadata transactions with a defined flush protocol. |
| Fragile bootstrap metadata | exFAT already has main and backup boot regions. | Two validated, mostly immutable superblocks with explicit disagreement rules. |
| Fragmentation | Extents reduce mapping overhead for contiguous ranges; they do not eliminate fragmentation or linear directory searches. | Extents now, indexing only after measurement. |
| Flash wear | COW and logging can improve write patterns but can also increase amplification through copying and cleaning. | Measure logical writes; no claim of better physical wear in v0.0.1. |
| Legacy names | exFAT already eliminates FAT-style 8.3 aliases. | UTF-8 names with explicit comparison rules. |
| Portability | A simple format alone does not create OS or firmware support. | Portable core and Linux FUSE first; separate adapters later. |

Discard is a device/transport capability, not a special on-disk encoding. Neither
availability nor effectiveness of discard is required for correctness. Historical
licensing concerns are background, not a claim about current legal status or a
reason to design a particular disk structure.

### 1.2 Target device contract

Target regular image files and USB/SD-style logical block devices whose controller
handles physical flash management. Raw NAND, erase-block management, bad-block
mapping, and physical wear leveling are out of scope.

The backend provides bounded reads, writes, capacity discovery, and a durable
flush. Writes can tear or be reordered before a flush. A successful flush makes
all preceding successful writes durable. Durably written bytes are assumed not
to disappear during later power loss.

Recovery guarantees depend on that contract. A controller that acknowledges
flush without retaining data can defeat it. Checksums may detect some resulting
damage; they cannot reconstruct arbitrary lost data.

### 1.3 Reference workload (D-006)

The user's reference device is a roughly **128 GB flash drive**, used for a
general-purpose mixture of small and large files. Treat "mixed use" as both
copying files and editing them directly, with no prescribed ratio or throughput
target. This is a testing baseline, not a format capacity limit.

Exercise large sequential copies, many small files, in-place edits, staged file
replacement, and near-full-volume allocation/reclamation. Report latency and
logical write volume for these workloads, especially the cost of synchronous
transactions. For illustrative sizing use decimal GB; derive actual filesystem
geometry from the backend's reported capacity.

Per D-018, v0.0.1 has no numeric throughput or mount-latency release threshold.
Measure and publish performance on these workloads; correct recovery takes
priority. Hangs, unbounded resource use, and algorithmic defects are still bugs.

## 2. Scope and deliberate tradeoffs

### Included in v0.0.1

- Image-backed core, formatter, inspector, read-only checker, recovery tool.
- Linux FUSE adapter only for v0.0.1 (D-011); one writable mount and one serialized
  operation stream. Windows/macOS adapters are not release requirements.
- Regular files, directories, stable inode identities, and linear directories.
- Extent mappings and a flat allocation bitmap.
- Sparse holes, including zero-filled reads beyond initialized content.
- Metadata checksums and a bounded metadata redo journal (selected in D-007).
- Atomic namespace changes that fit one transaction, including supported rename.
- A documented, crash-tested whole-file replacement workflow (D-002): stage a
  complete new file, sync it, and atomically publish it through rename.
- Incremental, recoverable reclamation for large truncates and deleted files.
  Complete eligible reclamation synchronously before success (D-019); do not add
  a deferred cleanup queue in v0.0.1.
- Case-insensitive, case-preserving UTF-8 names (D-009), nanosecond timestamp
  representation, explicit feature handling.
- Full Unicode canonical caseless matching (D-010) and conservative portable
  filename restrictions enforced even on Linux (D-012).
- Deterministic crash simulation and malformed-image tests.
- A full metadata/ownership check before normal writable mounting (D-016).
- Best-effort degraded read-only access to validated files after metadata
  corruption, with explicit errors where trust cannot be established (D-013).
- A persistent executable flag for regular files; other permissions and ownership
  are synthesized from mount options (D-014).

Sparse files move into the first version deliberately: explicit logical offsets
make holes easy to represent, and their semantics simplify writes beyond EOF.

### Deferred

- Data checksums (explicitly deferred by D-005), data repair, metadata replication
  beyond superblocks. Silent corruption in file contents may go undetected.
- Data journaling and atomic multi-block in-place overwrites. Whole-file
  replacement through a separate staged file and rename is included.
- COW, snapshots, compression, encryption, ACLs, xattrs, tags.
- Hard links, symlinks, special files, quotas, online resizing.
- Optional case-sensitive mode and alternative Unicode comparison policies.
- Directory/extent/free-space trees, concurrent mutation, background workers.
- Discard execution and any wear-leveling optimization.
- Native kernel drivers, Windows/macOS adapters, firmware integration.
- Automatic repair of arbitrary corruption.

Per D-004, mutating core operations wait for durable completion in this version.
Metadata transactions are synchronous; successful write chunks also flush their
data. This may be slow with many small operations. Application/kernel buffering
before a request reaches the core is outside this guarantee. Batching can come
later only as an explicit contract change with its own recovery tests.

## 3. Architecture

```text
mkfs / inspect / check / recover / image CLI       Linux FUSE adapter
                    \                              /
                     -------- libxffs ------------
                               |
                         block-device API
                               |
               image file / raw device / simulator
```

`libxffs` owns serialization, validation, allocation, namespace operations,
transactions, recovery, and file-handle lifetime. It has no FUSE dependency.
The adapter translates operating-system requests and errors only.

Backend interface:

- `capacity_bytes()`
- `read_at(offset, destination)`
- `write_at(offset, source)`
- `flush()`
- An exclusive writable-open mechanism appropriate to the backend.

All offsets and lengths are checked before I/O. Short I/O is handled explicitly;
errors are never converted into successful partial metadata writes. The simulator
models volatile writes separately from durable bytes.

The selected implementation language is Rust (D-015), with explicit binary
encoding and a thin FUSE binding. Language choice is a mutable implementation decision,
not part of the disk format. Never serialize a language-native struct by copying
its memory representation.

Per D-017, use existing Rust libraries for Unicode processing, checksums, FUSE,
and CLI parsing. Implement allocation, disk structures, transaction construction,
and recovery in the project. Choose and pin concrete crates during scaffolding;
no particular crate/version is selected by this document. Verify libraries against
format fixtures, and keep Unicode data versions tied to the format policy rather
than silently inheriting dependency upgrades.

Single-threaded execution does not eliminate open-file lifetime rules, partial
writes, rename semantics, or coordination with the kernel's caches. Start with
FUSE writeback caching disabled and conservative cache settings. Flush, release,
fsync, and fsyncdir must have separate documented adapter behavior.

## 4. Format conventions and limits

### 4.1 Common conventions

- All multi-byte integers are little-endian.
- Filesystem block size is fixed at 4,096 bytes for v0.0.1.
- Block numbers and logical offsets are unsigned 64-bit values.
- File size fields are unsigned 64-bit, but the first driver rejects sizes above
  `2^63 - 1` bytes and lower backend/platform limits.
- Compute offsets with checked arithmetic; validate both multiplication and sum.
- Unknown required features, impossible sizes, overlaps, and invalid pointers
  cause a controlled error before dereference or allocation.
- Readers bound resource use independently of values supplied by the image.
- Reserved bytes are written as zero; format-specific preservation rules are
  required before any future feature assigns them meaning.
- A sector is not assumed to be an atomic write unit. Metadata checksums cover
  the entire filesystem block and detect torn metadata with checksum limitations.

The first format manual must publish exact field offsets, checksum test vectors,
and limits before the writer is implemented. This document selects the design;
it is not a complete byte-level serialization specification.

### 4.2 Volume layout

All offsets are relative to the filesystem volume, not the whole physical disk.
A partition or bounded image slice is presented to the core as its own device.

| Region | Purpose |
|---|---|
| Block 0 | Primary superblock. |
| Fixed reserved metadata region | Journal controls/payload, allocation bitmap, inode table. |
| Allocatable region | File data, directory blocks, extent-list blocks. |
| Last filesystem block | Backup superblock. |

The formatter selects explicit non-overlapping ranges for the fixed metadata
region and records them in both superblocks. There is no arbitrary 1 MiB gap
inside the filesystem; partition alignment belongs to the enclosing disk layout.
Trailing bytes shorter than one filesystem block are outside the filesystem.
Volumes below 16 MiB are rejected by the initial formatter.

Formatting starts from an explicitly specified image or device and initializes
all metadata. It need not zero the whole data region: allocation and zero-exposure
rules prevent old bytes from becoming file content. Device formatting is always
an explicit user action, never a mount-time fallback.

### 4.3 Superblocks

Store magic, format major/minor, volume UUID, block size/count, region locations
and lengths, root inode identity, inode capacity, feature masks, checksum scheme,
and a checksum over the entire block with its checksum field zeroed.

Superblocks are immutable during ordinary operation. Free-space counts, journal
state, and last-mount times do not live here. This avoids a mutable root-pointer
protocol and repeated superblock writes.

Mount independently validates both copies against backend capacity:

- Two valid matching copies: proceed.
- Exactly one valid copy: permit read-only inspection; require an explicit
  offline restoration of the other copy before writable mounting.
- Two valid copies that disagree on authoritative fields: refuse normal mount.
- Neither valid: refuse normal mount.

The backup location is derived from the presented volume capacity. Resizing or
embedding an image in a different-sized device is unsupported unless the backend
presents its original bounded size. A newer revision must design resizing rather
than guessing which copy is newer. Backup superblocks do not back up the inode
table, directories, or bitmap.

### 4.4 Feature negotiation

Use three 64-bit feature masks:

- Compatible: unknown bits are safe for read/write only because that feature's
  definition guarantees old writers preserve validity.
- Read-only-compatible: unknown bits prohibit writes but permit reading.
- Incompatible: unknown bits prohibit ordinary mounting entirely.

Unknown major versions are rejected. Minor versions are accepted only under
published compatibility rules and feature requirements. Reserved bytes alone do
not make a format extensible. Future features must define preservation and
validation behavior, including any new metadata record types.

## 5. Metadata structures

### 5.1 Common metadata block header

Every ordinary metadata block has a 64-byte header containing a type, header
version, physical block number, owner identity where applicable, last transaction
ID, payload length, flags, and CRC32C. The exact byte layout is fixed in the format
manual. Unused bytes are zero and covered by the checksum.

Checksums cover the whole block with the checksum field zeroed. Binding the block
number and owner into the checked bytes helps detect misplaced blocks. Checksums
are accidental-corruption detection, not authentication, and cannot prove that
an otherwise valid block is the newest version.

Superblocks and journal controls have their own fixed headers and whole-block
checksums. Journal payload images retain ordinary metadata headers.

### 5.2 Allocation bitmap

Use one bit per filesystem block, packed into checksummed bitmap blocks. Fixed
metadata, both superblocks, and out-of-range padding bits are permanently marked
unavailable. Bitmap block headers reduce usable bitmap payload and must be
included in sizing calculations.

The bitmap records allocations of both file data and dynamic metadata. Ownership
is derived by traversing inodes, directories, and extent-list blocks. A memory-only
allocation cursor and contiguous-run search are sufficient initially.

Treat reservations as transaction-local until commit. Never persist the bitmap
before the transaction that establishes the corresponding ownership.

At 4 KiB blocks, a 1 TiB volume needs approximately 32 MiB of bitmap bits, plus
headers. This is acceptable for the target hobby implementation, but not a claim
of good embedded-memory scaling. Cache bitmap blocks with an explicit bound.

### 5.3 Inode table

Per D-008, use a fixed inode table with formatter-selected capacity rather than
storing inodes inside directory records or growing inode storage dynamically.
Use 256-byte inode records inside checksummed metadata blocks. With the 64-byte
header, each block holds 15 records and 192 bytes of zero padding.

Each inode contains:

- Allocation state, type, and generation number.
- Executable flag for regular files (D-014).
- Logical size and allocation accounting.
- Creation, modification, and metadata-change timestamps.
- Parent identity for directories.
- Four inline extent descriptors and an extent-list head if needed.
- Reclamation state and a persistent progress marker when needed.

The exact layout must fit these fields before it is frozen. Access time is not
persisted in this version. Persist one executable flag for regular files (D-014);
ownership and remaining permissions are synthesized from mount options. Directory
search permissions are also synthesized. Full POSIX ownership, separate per-class
execute bits, and portable security enforcement are not promised by the format.

The format/adapter manual must specify how create and chmod set this bit and how
getattr maps it to the permitted owner/group/other execute bits. Metadata changes
to the flag are journaled with ctime. Per D-020, honor the stored executable flag
by default subject to mount modes, and provide an explicit noexec mount option
that suppresses direct execution without clearing stored flags. Do not
silently claim to preserve permission distinctions the format cannot represent.

Inode identity is `(table index, generation)`. Reusing a slot increments its
generation; an exhausted generation retires the slot rather than wrapping.
Directory references include both components. Hard links are unsupported.

`mkfs` accepts an inode count and reports its metadata cost. A suggested default
is one inode per 64 KiB of volume capacity, with a documented minimum. Users can
choose another count. Running out of inodes returns ENOSPC even with free blocks;
`statfs` reports both resources.

### 5.4 Extents and holes

Each extent is `(logical_start_block, physical_start_block, block_count)`, using
three 64-bit fields. Extents are sorted by logical start, have nonzero lengths,
never overlap logically, and map only valid allocatable blocks. Adjacent mappings
are merged when both logical and physical ranges are contiguous.

Unmapped logical ranges below EOF are holes and read as zero. EOF is determined
by inode size, not by the last extent. File data blocks have no metadata header
and no checksum in v0.0.1.

Overflow extents use a singly linked chain of checksummed metadata blocks owned
by that inode. Readers check ownership, bounds, duplicate visits, and cycles.
Memory-only indexes may accelerate access. There is no on-disk tree.

Set the initial implementation limit to 65,536 extents per inode, including
inline extents. The format manual derives the corresponding maximum chain length
from the finalized block encoding. Exceeding a supported mapping limit fails with
EFBIG before making an unrepresentable change. It must not silently truncate
mappings. Inserting an extent uses local block edits/splits rather than repacking
the entire chain; transaction preflight still applies.

### 5.5 Directories and names

Directories contain checksummed blocks of variable-length records. Records never
cross block boundaries and include record length, live/free status, inode index,
inode generation, and filename byte length. Free records can be reused; no online
directory compaction is needed initially.

Rules:

- Names are valid UTF-8, from 1 to 255 bytes.
- NUL and `/` are prohibited; `.` and `..` are reserved and synthesized.
- Comparison is case-insensitive and independent of host locale (D-009).
  `Report.txt` and `report.txt` address the same name within a directory.
- Preserve original filename spelling for display. Generate comparison keys in
  memory rather than storing a second spelling in each directory record.
- No two live entries may have equivalent comparison keys. Creation rejects a
  conflicting name; lookup uses the same equivalence rule.
- A case-only rename updates the stored spelling atomically without unlinking
  or orphaning the inode as if it were a different destination file.
- Per D-010, compare using full Unicode case folding and canonical normalization,
  following Unicode canonical caseless matching. `Straße.txt` and `STRASSE.txt`
  collide, as do composed and decomposed encodings of the same accented letters.
  Preserve original stored bytes; normalization is part of comparison, not a
  silent spelling rewrite. Do not strip accents or apply compatibility
  normalization or locale-specific casing.
- The format must pin the exact comparison algorithm and Unicode data release
  before any image writer is implemented. Drivers cannot substitute host locale
  rules or silently upgrade the tables. Include golden comparison vectors.
- Store or unambiguously derive the comparison-policy identifier from the format
  revision; refuse normal mounts with an unsupported policy. Any future policy
  migration must check for newly equivalent names before changing the policy.
- The 255-byte limit applies to stored UTF-8 names. Bound comparison-key memory
  separately, accounting for transformations that expand the name.

Lookup and insertion search linearly. Large directories may be slow. Extents do
not change that fact. Readdir cookies and mutation behavior are defined in the
adapter; they must not expose invalid memory or loop indefinitely.

Per D-012, enforce a conservative portable-name policy in the core on every host.
Reject invalid names on creation and rename; never silently sanitize them. The
exact list of prohibited characters, reserved names, trailing characters, and
length rules is a required format-manual deliverable before the writer is built.
It remains unspecified here rather than being inferred from the broad decision.
Differences between host and XFFS comparison rules can still affect portability;
acceptance does not imply lossless round trips through every host. Conformance fixtures must
exercise non-ASCII case variants, the chosen normalization behavior, equivalent
name rejection, case-only rename, and identical lookup across supported hosts.

### 5.6 Timestamps

Store signed 64-bit UTC seconds from the Unix epoch and an unsigned nanosecond
fraction in `[0, 999999999]`. This is representation precision, not a promise that
the clock or device measures nanoseconds. No local UTC offset is stored.

Creation time remains stable; content changes update mtime; metadata changes
update ctime. Define `utimens` behavior in the adapter. Clock movement backward
does not affect journal ordering, which uses integer transaction IDs.

## 6. Transaction and recovery protocol

### 6.1 What is atomic

A transaction atomically installs a set of metadata block images after recovery.
The set includes every affected inode-table, directory, extent-list, and bitmap
block. Transaction construction uses private copies; uncommitted metadata is
never written to its home location.

Namespace operations are one transaction when supported by the bounds. Large
file writes are a sequence of committed chunks. In-place overwrites of existing
file data are not atomic, even if their timestamp update is journaled.

### 6.2 Fixed journal

Reserve two control blocks and 512 payload blocks: 514 blocks total. Each metadata
image occupies a descriptor block plus a 4 KiB image block, allowing at most 256
distinct modified metadata blocks per transaction. This is a deliberately simple
initial encoding; optimizing descriptor packing is deferred.

Descriptors contain transaction ID, ordinal, target block, metadata type, and
checksum. The commit control includes transaction ID, record count, and a CRC32C
over the ordered descriptors and images, in addition to its own checksum.

Before starting I/O, preflight the distinct metadata-block budget and all needed
space. Reject or split the operation at a defined semantic boundary if it cannot
fit. An atomic namespace operation is never silently split. Use a documented
E2BIG result if its transaction would exceed the fixed journal budget.

Control blocks record `EMPTY` or `COMMITTED`, a monotonically increasing control
sequence, volume UUID, and transaction ID. Counters must not wrap. Initial
formatting writes valid EMPTY controls. A torn control is detectable by its
checksum, subject to the limits of CRC detection.

### 6.3 Commit sequence

Only one transaction may be active. The previous transaction is checkpointed and
the journal made reusable before the next payload is written.

1. **Build:** reserve space in memory and construct all replacement metadata.
   Include block initialization and changes to allocation ownership.
2. **Initialize data:** write newly exposed data, including zeros around partial
   writes into newly allocated blocks, and flush. Existing-data overwrites follow
   the file-write rules below.
3. **Prepare journal:** write all descriptor/image pairs and flush. Do not touch
   their final metadata locations.
4. **Commit:** write a COMMITTED control with a higher sequence into the older
   control slot and flush. This is the metadata transaction's durable commit.
5. **Checkpoint:** write the committed images to their home blocks and flush.
6. **Retire:** write an EMPTY control with a higher sequence into one slot and
   flush, then write EMPTY with another higher sequence into the other slot and
   flush. Both slots must durably be EMPTY before payload reuse.
7. **Publish success:** return success after retirement in the initial
   implementation. Later acknowledgment or batching changes require new tests.

Making both controls EMPTY before reuse prevents an old COMMITTED control from
remaining as a fallback while its payload is being overwritten. Never recycle
journal payload solely because home writes were issued.

An I/O or flush failure after writes begin makes the mount fail further mutation
and require recovery. Do not attempt another transaction on an uncertain journal.
An operation that returned an error after commit may still have taken effect;
errors cannot promise rollback after a durable commit.

### 6.4 Recovery state machine

Before ordinary metadata traversal, validate the journal controls:

- Select the valid control with the highest sequence.
- Equal sequences with conflicting contents, no valid controls, or invalid state
  transitions result in a recovery error.
- Selected EMPTY: do not replay payload. Before writable reuse, establish both
  slots as durably EMPTY if retirement was interrupted.
- Selected COMMITTED: validate the complete referenced payload, then replay every
  image to its home location, flush, and retire both controls as above.
- Invalid payload for a selected COMMITTED record: report corruption and refuse
  ordinary mount. Do not fall back to an older EMPTY state to hide the error.

Validate record counts, duplicate targets, target bounds, metadata types,
checksums, and that no target is a superblock or journal block. New dynamic
metadata may be allocated by the transaction itself; validation must not reject
it merely because the pre-replay bitmap still marks it free.

Replay is idempotent. A crash during replay repeats it. A crash during retirement
can either replay the old committed images again or observe EMPTY, depending on
which valid control became durable. Home metadata is durable before either EMPTY
control is issued.

These rules cover interrupted writes under the device contract. They do not
provide rollback protection against arbitrary later corruption of already durable
control blocks, deliberate tampering, or replacement with old valid images.

### 6.5 Crash outcomes

| Failure point | Expected recovery |
|---|---|
| Before durable commit | Ignore incomplete journal; old metadata remains authoritative. Unreferenced data writes are harmless to allocation consistency. |
| Commit write torn | A valid committed control is replayed; otherwise the prior EMPTY control wins. |
| Commit durable, checkpoint incomplete | Replay all committed metadata images. |
| Checkpoint durable, retirement interrupted | Replay idempotently or observe EMPTY. |
| Payload reuse interrupted | Both prior controls were already durably EMPTY; incomplete new payload is ignored. |
| Selected committed payload corrupt | Fail safely; checksums do not repair it. |

### 6.6 Read-only recovery

A genuinely read-only backend must never be modified to make it mountable.
For a committed valid journal, build a bounded in-memory block overlay containing
its metadata images. Read all metadata through that overlay, presenting the same
logical state that replay would install. Validate the overlay before exposing it.

The read-only checker uses the same mechanism. An explicit forensic raw-view
option may inspect unrecovered blocks, but must label the view inconsistent and
must not expose it as an ordinary mounted filesystem.

Pending reclamation remains allocated on read-only mounts. Writable mounts finish
recovery and reclamation before accepting normal mutations.

### 6.7 Degraded read-only access after corruption (D-013)

Attempt to expose validated files read-only when localized metadata damage blocks
healthy mounting. This is a visibly degraded view, not a claim that the volume is
healthy or that file contents have been checksum-verified. Report affected paths
and structural errors through mount diagnostics and the checker.

- Bootstrap geometry, comparison policy, and a coherent recovery state must be
  established first. Unknown geometry or an invalid committed journal cannot be
  bypassed by pretending the volume is clean. Some damage therefore prevents even
  a degraded mount; forensic inspection remains separate.
- Validate each exposed object's directory ancestry, inode generation, extent
  mappings, metadata checksums, bounds, and relevant ownership relationships.
  Quarantine ambiguous mappings and shared-block conflicts. Damage to global
  metadata can make otherwise checksum-valid objects untrustworthy.
- Return explicit I/O errors for untrusted accesses. Never synthesize empty files,
  zero-fill unreadable data as if it were a sparse hole, or present a damaged
  directory as successfully enumerated in full.
- Perform no backend writes, on-disk replay, reclamation, or automatic repair.
  A valid committed journal can still be applied through the read-only overlay.
- If corruption appears during a writable session, stop admitting mutations and
  quiesce operations. Rebuild a coherent read-only view before serving further
  affected reads; invalidate suspect caches/handles. If that cannot be done in
  place, fail affected operations and require a degraded read-only remount.

Per D-016, normal writable mounting requires the full check below. Degraded-mode
validation and diagnostics remain separate required work and must be tested
independently of crash replay.

### 6.8 Full validation before writable service (D-016)

Every requested normal writable mount performs a full metadata and ownership
scan. First validate bootstrap structures and the journal, and build the logical
post-replay view using the read-only recovery overlay. Run the checker against
that view before modifying the backend. This avoids incorrectly diagnosing a
partially checkpointed but recoverable transaction as structural corruption.

The scan covers the complete inode table, allocation bitmap, reachable directory
and extent metadata, plus orphan/pending-reclamation ownership. Those explicit
recovery states are valid if their invariants hold; they are not unexplained leaks.
If validation succeeds, replay the journal, finish pending reclamation through
validated bounded transactions, and then admit normal mutations. Hold the backend
lock throughout so cooperating writers cannot invalidate the checked view.

If validation fails, do not admit writable service or automatically repair the
damage. Attempt the degraded read-only path under section 6.7. Continue checking
metadata when accessed even after a successful mount-time scan; later failures
still trigger the corruption policy.

At the proposed default density, the reference 128 GB volume's inode table alone
requires roughly 533 MB of reads, plus bitmap and other metadata. Accept this
initial mount cost and measure it. The scan does not read and checksum all user
file contents; D-005 still defers data-integrity checksums.

## 7. Operation semantics

### 7.1 Create and mkdir

Allocate an inode, initialize its state and any directory block, and install the
parent entry in one metadata transaction. Parent mtime/ctime changes belong to
that transaction. No directory entry may become durable independently of its
inode and allocation state.

### 7.2 Read, write, and extension

Reads stop at EOF and synthesize zeros for holes.

Writes are chunked by both data limits and preflighted metadata journal capacity.
Each successful chunk establishes its mappings, allocation bits, and resulting
size in one transaction. Return the completed byte count when a later chunk fails;
do not report bytes merely copied into a temporary buffer.

A partial write into a newly allocated block first initializes all bytes that can
be exposed. Gaps between old EOF and a write are holes or explicitly zeroed bytes.
If old EOF falls within an allocated block, initialize the newly exposed tail
before committing an increased size. An unallocated range must never expose data
left by a previously deleted file.

An overwrite of already allocated data can leave old/new/torn content after a
crash. Metadata checksums do not detect this. A successful synchronous write in
this version flushes its data before returning, but does not promise atomicity
against an interruption before success.

### 7.3 Truncate and reclamation

Growing a file changes its size and exposes zeros, following the same partial-tail
rules as writing beyond EOF. New full-block holes need no data allocation.

Shrinking a large file cannot necessarily free every mapping in one journal
transaction. Instead:

1. Atomically publish the smaller size and a pending-reclamation marker.
2. Keep unreachable trailing mappings allocated while the marker exists.
3. Reclaim those mappings and overflow blocks in bounded transactions, advancing
   persistent progress together with bitmap changes.
4. Clear the marker when all trailing storage is reclaimed.

Marked trailing mappings are a documented temporary exception to the usual EOF
mapping invariant. The checker verifies that they remain owned and allocated.
The initial driver completes this work before returning successful truncate and
before another mutation of that inode. Recovery resumes interrupted work before
writable service.

Reclaim from the tail and update existing mapping blocks so that freeing storage
does not itself require free data blocks or a newly allocated extent-list block.
Every reclamation transaction must fit the reserved journal even on a completely
full volume. Failure halfway through leaves an explicit pending state, never a
bitmap-only free whose ownership record still treats the blocks as live.

Do not zero bytes that are still below the old EOF merely to prepare a shrink:
that would alter the old file before the shrink commits. Once shrunk, any future
extension must zero the retained partial block's newly exposed bytes before
publishing the larger size.

### 7.4 Unlink, rmdir, and open handles

Unlink atomically removes the name and marks the inode orphaned. A currently open
file remains accessible through its handle until the last handle closes. Its
blocks cannot be reused while that handle exists.

Per D-019, when unlink leaves no open handle, finish reclamation in bounded
transactions before returning success. When handles remain, defer freeing only
until their lifetime ends, then reclaim synchronously on final close. Free the
inode slot only after its storage has been reclaimed. After a crash, process
memory and handles are gone; mount recovery scans the fixed inode table and
finishes orphan reclamation before writable service. A linear scan is acceptable.
Cleanup errors can follow a committed unlink; report the error without implying
that the original directory entry was restored. The adapter must document any
close/release error-reporting limitations and retain diagnostics.

Rmdir requires an empty directory, respects open directory handles, and never
removes the root. Cross-directory operations update directory parent identities.

### 7.5 Rename

Support ordinary rename, including replacement with compatible file/directory
types, as one metadata transaction. Replacing a directory requires it to be empty.
Replacing a file marks the displaced inode orphaned; reclaim it in separate
bounded transactions after the atomic rename commit. Per D-019, finish eligible
reclamation before reporting success, except where open handles retain the old
inode. Cleanup failure after commit does not undo the rename.

The source removal, destination installation, displaced inode state, parent
identity, and parent timestamps all belong to the same transaction. Reject moves
that would create a directory cycle. Handle same-name/same-object cases explicitly.
Unsupported exchange/whiteout variants return a documented unsupported error.

After a crash, an interrupted rename is wholly before or wholly after at the
namespace level, assuming valid recovery. A successfully acknowledged rename is
after. Neither outcome implies atomic replacement of the file's data contents.

#### Whole-file replacement workflow (D-002)

The user selected safe whole-file replacement while accepting that ordinary
in-place writes can be interrupted. The proposed implementation uses existing
file and rename operations, rather than a new on-disk transaction type:

1. Begin with a durable existing destination, if one exists. Create a uniquely
   named temporary file in the same directory on the same filesystem.
2. Write the entire replacement, check every result, and successfully fsync the
   temporary file. Do not modify it after this point until publication completes.
3. Rename it over the destination in one namespace transaction.
4. Successfully fsync the parent directory before reporting durable publication.

Before publication commits, the destination still refers to the old file. After
commit, it refers to the fully staged replacement. After successful completion,
the new destination must survive recovery under the device contract. A failure
after commit may mean publication succeeded despite the reported error; inspect
the recovered namespace rather than assuming rollback.

A crash before publication can leave a temporary file. Do not automatically
delete files merely because their names look temporary. Existing handles to the
old inode continue to see that inode until closed; reclaim it under the orphan
rules. Enough space for staging both versions is required, and ENOSPC must leave
the original destination intact before publication.

This guarantee requires the workflow; it does not make arbitrary application
saves or in-place overwrites atomic. Test old/new content identity at the
destination after every simulated interruption, plus temporary-file leftovers,
open old handles, ENOSPC, and fsync failures. A convenience tool/API remains an
optional interface decision.

### 7.6 fsync, errors, and mount lifecycle

`fsync` flushes relevant data and finishes any pending metadata work. A simple
whole-filesystem flush is acceptable initially. `fsyncdir` provides the analogous
namespace durability guarantee. Never return success for an unimplemented no-op.

Because metadata operations are synchronous initially, their successful return
already establishes durable namespace changes. `fsync` remains a real operation
and part of the public contract for later evolution.

Unmount drains operations, completes pending work, flushes, and reports failures.
A clean-unmount flag is not used as a substitute for journal validation. On
read/write errors or metadata corruption, stop mutation rather than trying to
continue with uncertain allocator state. For corruption, attempt the degraded
read-only path in section 6.7; do not expose uncertain cached state as healthy.

Acquire an exclusive backend lock for writable mounting and reject another
cooperating writer. This is not protection from an unrelated program writing the
raw device; external modification while mounted is unsupported.

## 8. Tools and validation

### Required tools

- `mkfs.xffs`: create an image or explicitly format a selected device; print
  layout, inode capacity, journal capacity, and format revision.
- `xffs-inspect`: dump decoded structures, allocation ownership, and journal state
  without trusting pointers blindly.
- `xffs-check`: read-only structural and allocation consistency checker, using
  recovery overlay when needed. Return distinct clean/corrupt/I/O statuses.
- `xffs-recover`: explicit writable replay, reclamation, and narrowly defined
  superblock-copy restoration. Refuse unrelated corruption; this is not general
  repair.
- `xffs-image`: exercise create/read/write/rename/truncate/unlink without FUSE.
- `mount.xffs`: Linux FUSE adapter with read-only and read/write modes.

### Checker invariants

- All ranges are in bounds and fixed regions do not overlap.
- Metadata checksums, block identities, owners, and encodings validate.
- Every reachable or reclamation-owned block is allocated.
- No block is owned by two unrelated objects.
- Every allocated dynamic block has an explained owner, including orphans and
  pending reclamation; unexplained allocations are reported as leaks.
- Every live directory entry names an allocated inode with matching generation.
- Live namespace inodes have the expected reference count; there are no hard links.
- Parent identities are coherent, root is valid, and directory/extent chains do
  not cycle.
- Extents are ordered, valid, non-overlapping, and compatible with inode state.
- Journal targets/counts are valid and replay is idempotent.

Zero exposure and durability also require behavioral tests; a static ownership
walk cannot prove that file contents were initialized correctly.

### Simulator and test strategy

The simulator keeps volatile writes and a durable image separate. It supports:

- Dropping unflushed writes and persisting subsets in different orders.
- Tearing writes at chosen offsets, including journal controls and payload.
- Successful flushes that preserve all prior writes.
- Injected read/write errors, short I/O, ENOSPC, and flush failures.
- A fixed random seed and a compact operation/event trace for reproduction.

For small workloads, crash at every write and flush boundary and systematically
exercise chosen tear/reordering patterns. Supplement with randomized long traces;
do not call sampled reorderings exhaustive coverage.

Compare namespace and contents with an in-memory reference model. For interrupted
operations, the oracle admits exactly the documented alternatives, including
partial writes and non-atomic in-place overwrites. Successful fsync/namespace
operations constrain the permitted durable outcomes.

Essential cases: empty/full volumes, inode exhaustion, severe fragmentation,
extent overflow boundaries, maximum names, non-ASCII names, duplicate names,
invalid UTF-8, Unicode name equivalence and case-only rename, partial-block
writes, holes, shrink/regrow, multi-transaction
reclamation, rename replacement, open-unlink, interrupted recovery, corrupt
metadata, malformed arithmetic, and read-only recovery without any backend write.

## 9. Full development order

Each phase leaves a runnable artifact. Do not start the next dependent write path
until its listed exit criteria pass. Breaking experimental images is acceptable;
record revisions and keep small fixtures for each supported revision.

| Phase | Work and deliverable | Completion check |
|---|---|---|
| 0. Repository and decisions | Initialize Git; choose language and dependency versions; add build command, license choice, format-status notice, and this design. Create directories for core, tools, adapter, docs, and fixtures. | Clean checkout builds a minimal core/tool executable; no claim of format stability. |
| 1. Device abstraction and simulator | Implement bounded image I/O, checked offsets, short-I/O handling, flush, exclusive open, and volatile/durable simulator. | Known write/flush/crash sequences produce expected durable bytes; failures cannot escape bounds. |
| 2. Byte-level format manual | Freeze initial encodings, field offsets, checksums, UUID rules, journal control selection, counter limits, extent caps, and error mappings. Work through crash traces on paper. | Every field has a width and validity rule; worked create/rename/reclaim transactions fit the journal. |
| 3. Encoding and parser | Implement checksums, encode/decode, superblock/metadata/control validation, and resource bounds. Add golden binary fixtures. | Golden bytes match the manual; corrupt/truncated input gives errors without panics, unbounded allocations, or out-of-range I/O. |
| 4. Formatter and inspector | Write fixed regions, both superblocks, empty controls, bitmap, root inode/directory; expose decoded layout. | Two independent reads agree on layout; minimal/default/custom inode-count images validate; root is inspectable. |
| 5. Read-only core and checker | Implement inode/extent/directory traversal, lookup, reads, holes, statfs, and ownership checks using generated fixtures. | Read known files and >4 GiB logical fixtures; detect duplicate ownership, cycles, stale generations, and bad checksums. |
| 6. Read-only FUSE milestone | Implement mount, lookup, getattr, readdir, open, read, statfs, and explicit read-only errors. | Ordinary tools traverse and read fixture images; malformed images fail cleanly; no write reaches the backend. |
| 7. Journal and recovery engine | Implement preflight, descriptor/image construction, controls, commit, checkpoint, retirement, writable replay, and read-only overlay. Exercise synthetic metadata transactions before file mutations. | Crash matrix holds, including torn controls, interrupted replay/retirement, payload reuse, and repeated recovery. |
| 8. Allocator and basic mutation CLI | Add in-memory reservations, inode allocation, block allocation, create, mkdir, and bounded file writes through the journal. | CLI can create/remount/read; ENOSPC leaves valid ownership; acknowledged operations survive simulated loss. |
| 9. Extents and sparse files | Add overflow chains, fragmentation, partial writes, holes, extension, and chunked large writes. | Verify a real >4 GiB payload when storage permits, plus sparse/offset tests in routine CI; no stale-byte exposure. |
| 10. Reclamation and handles | Add shrink, orphan state, incremental freeing, inode reuse/generations, open-unlink, close, rmdir, and mount-time resumption. | Crash every reclamation step; no double frees/leaks after completion; old handles never alias reused inodes. |
| 11. Atomic rename | Add same/cross-directory rename, replacement, cycle checks, and bounded preflight. | Crash tests yield only valid before/after namespaces; displaced open files remain usable; oversize transactions fail before mutation. |
| 12. Read/write FUSE | Wire proven core ops; implement the full pre-write-mount validation gate, truncate/setattr/utimens, fsync/fsyncdir, error translation, handle release, and cache policy. | Every writable mount passes full metadata/ownership validation; run copy, compare, rename, remove, sparse, and open-unlink workloads; adapter reports real durability errors. |
| 13. Integrated crash and fuzz campaign | Combine randomized operations, reference model, parser fuzzing, I/O failures, remount, and recovery interruption. Implement and test degraded read-only mounting, quarantine, diagnostics, and transition from writable service. Add regressions for every found defect. | Reproducible suites pass with checker invariants after every recovered workload; damaged regions return errors, independently validated files remain readable where possible, degraded mounts issue no writes, and no unresolved ownership or durability bugs remain. |
| 14. Removable-device experiments | Use an explicitly selected expendable USB device; format, mount, copy/verify, clean remount, then controlled removal experiments. | Record device/backend/flush behavior and results. Simulator guarantees and observed hardware behavior are reported separately. |
| 15. v0.0.1 release | Freeze revision, document commands, limitations, recovery contract, supported sizes, known performance, and reproducible fixtures. | Fresh checkout reproduces the demo and core tests; release notes clearly say experimental and specify image compatibility. |

Phase 2 may adjust parameters if a worked transaction does not fit. That is a
normal design iteration. Return to the format manual whenever implementation
finds an ambiguity; do not let the writer silently define a second specification.

### 9.1 Suggested repository layout

```text
docs/
  design.md
  on-disk-format.md
  recovery.md
  testing.md
crates/
  xffs-core/
  xffs-tools/
  xffs-fuse/
tests/
  fixtures/
  crash/
  workloads/
```

The existing documents can move into `docs/` when scaffolding begins. No need to
create empty packages just to satisfy this diagram.

### 9.2 Milestones worth celebrating

- First decoded image: formatter and inspector agree.
- First mount: browse a read-only fixture through the OS.
- First durable mutation: create a file, simulate a crash, recover it.
- First large file: verified contents beyond the FAT32 limit.
- First adversarial success: interrupt rename and reclaim at every tested boundary.
- First real stick: copy files, eject, remount, and verify.

## 10. After v0.0.1

Choose follow-up work based on what was enjoyable or measurably limiting:

1. **Integrity:** data checksums, scrub, and then explicit redundancy/repair if
   desired. Decide checksum granularity and update ordering before changing extents.
2. **Performance:** measure journal bytes per logical write, bitmap search work,
   mount scans, fragmented reads, and large-directory lookup. Optimize one measured
   bottleneck at a time.
3. **Portability:** add another adapter and test naming, timestamp, handle, and
   cache semantics against that host. Keep the disk format host-independent.
4. **Discard:** add optional batching only after frees are durable and no live
   reference remains. Serialize submission against reallocation; never discard
   a range that has been allocated again. Unsupported discard remains harmless.
5. **Different persistence design:** prototype COW/log structuring as a deliberate
   format revision or separate experiment. Snapshots do not fall out of padding
   fields in a redo-journal format.
6. **Conveniences:** xattrs, compression, encryption, richer permissions, and
   directory indexing each need independent semantics and compatibility rules.

Do not pre-build every future feature. The journal and simple fixed structures
are worthwhile even if this project never grows beyond a small experimental FS.

## 11. References

These are comparison and design references, not dependencies or claims that XFFS
implements another filesystem's exact protocol.

- [Microsoft exFAT specification](https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification): large-file support, backup boot region, metadata checksums, allocation bitmap, and TexFAT distinction.
- [Linux ext4 journal documentation](https://docs.kernel.org/filesystems/ext4/journal.html): write-ahead metadata journaling, commit, and checkpoint concepts.
- [Linux F2FS documentation](https://docs.kernel.org/filesystems/f2fs.html): flash-aware design tradeoffs and cleaning overhead.
