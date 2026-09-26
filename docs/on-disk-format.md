# XFFS experimental disk format 1.0

This is the authoritative format for this prototype. All integers are unsigned
little endian unless stated otherwise. Blocks are 4096 bytes. No native Rust
structure is serialized. Unspecified bytes, padding and reserved fields MUST be
zero. Checksums detect accidental damage; they do not authenticate hostile media.
All arithmetic must be checked before use. Trailing incomplete blocks are ignored.

## Decisions and engineering defaults

User decisions: metadata redo journaling, fixed inode capacity, portable preserved
names, executable flags, complete recovered-view validation, read-only recovery,
and rejection of corruption. Engineering defaults fixed by revision 1.0: the
encodings below, 512 payload blocks, minimum 16 MiB, one inode per 65536 bytes
(minimum 256), and a 128 MiB reader working-memory budget. These are experimental,
not a promise of compatibility with future revisions. Storage durability remains
as specified in [storage-contract.md](storage-contract.md).

## Common metadata block header

Every metadata block has this 64-byte header (including superblocks, bitmap,
inode table, directories, extent blocks, journal controls and descriptors).

| Offset | Bytes | Value |
| --- | --- | --- |
| 0 | 8 | ASCII `XFFSMETA` |
| 8 | 2 | type: super=1, control=2, descriptor=3, bitmap=4, inodes=5, directory=6, extents=7 |
| 10 | 2 | major=1 |
| 12 | 2 | minor=0 |
| 14 | 2 | reserved |
| 16 | 8 | physical home block number |
| 24 | 8 | owner inode index (zero for non-inode metadata) |
| 32 | 8 | owner generation (zero for non-inode metadata) |
| 40 | 4 | CRC32C of all 4096 bytes with these four bytes zero |
| 44 | 4 | used payload bytes, at most 4032 |
| 48 | 16 | reserved |

Unused payload bytes are zero. Journal images carry the original home identity,
not the physical payload location. Raw file data blocks have no header/checksum.
An inode identity is (index u64, generation u64); generation zero is invalid for
an allocated inode. Root is (0,1). Reuse increments generation, never wraps.

## Geometry and superblocks

Let B=floor(image bytes/4096), I=requested inode capacity or max(256,floor(B/16)).
B must be at least 4096; I at least 256. Region order is fixed:

- Block 0: primary superblock.
- Blocks 1,2: journal controls.
- Blocks 3..515 (exclusive): 512 journal payload blocks.
- Bitmap starts 515; length M=ceil(B/32256) blocks (4032 bytes of bits each).
- Inode table starts 515+M; length T=ceil(I/15) blocks.
- Allocatable region starts A=515+M+T; ends before B-1.
- Backup superblock is B-1. Require A < B-1.

Super payload (offsets from block start): UUID bytes 64..80; B at 80; I at 88;
M at 96; inode-table start at 104; T at 112; A at 120; comparison policy u32=1
at 128; compatible feature u32=0 at 132; read-only-compatible u32=0 at 136;
incompatible u32=0 at 140. Used payload=80. All unknown revisions, policies or
feature bits are rejected, including purported compatible bits in this revision.
Layout is recomputed and compared. Copies must agree in all payload fields;
physical identity and checksum naturally differ. Two valid conflicting copies
are corruption, not a freshness election. One valid copy permits read-only
mounting with a diagnostic. Neither valid means rejection. Unsupported revision
or policy in either checksum-valid copy is rejected, not hidden by fallback.

16 MiB: B=4096, I=256, M=1, T=18, A=534; allocatable blocks=3561,
backup=4095. Decimal 128 GB: B=31,250,000, I=1,953,125, M=969,
T=130,209, A=131,693; allocatable blocks=31,118,306, backup=31,249,999.
These sums include both superblocks, controls and payload region.

## Bitmap and inode table

Bitmap payload is always 4032 bytes. Bit b (least significant bit first) records
allocation of absolute block b. All reserved-region blocks and backup are set.
Bits beyond B are zero. Every set allocatable bit must have exactly one owner;
every referenced block must be set. No unexplained allocation is accepted.

Inode-table payload is always 3840 bytes: 15 slots of 256 bytes. Slots outside
capacity and unused slots are entirely zero. A live slot has these offsets:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | inode index, matching its table slot |
| 8 | 8 | generation, nonzero |
| 16 | 1 | kind: 1 regular, 2 directory |
| 17 | 1 | state: 1 linked, 2 orphan, 3 pending reclamation |
| 18 | 1 | executable: 0 or 1 (directories must use 0) |
| 19 | 5 | reserved |
| 24 | 8 | logical size in bytes |
| 32 | 8 | allocated blocks, including overflow blocks |
| 40 | 8 | parent index |
| 48 | 8 | parent generation |
| 56 | 8 | creation seconds since Unix epoch |
| 64 | 8 | modification seconds since Unix epoch |
| 72 | 8 | change seconds since Unix epoch |
| 80 | 8 | overflow head; zero means absent |
| 88 | 4 | total extent count, at most 65536 |
| 92 | 4 | reserved |
| 96 | 96 | four inline 24-byte extents |
| 192 | 64 | reserved |

Timestamps have zero nanoseconds and fit signed i64. Parent identities are valid
allocated directories for linked inodes; root names itself. No hard links. Each
non-root linked inode has exactly one directory entry. Orphan/pending inodes
have parent (0,0), are regular files, have no namespace references, but retain
ordinary complete mappings and ownership. Pending state reserves storage for a
future reclamation engine; partial reclamation must update mappings/accounting
atomically. Neither state is exposed or reclaimed by the reader.

An extent is logical start block u64, physical start block u64, length u64 > 0.
Extents are ordered, nonoverlapping in logical space, wholly within allocatable
space physically, and end no later than ceil(size/4096). Absent regular-file
mappings are holes. Directory mappings have no holes; directory size is a
multiple of 4096. Empty inodes have no extents. Unused inline slots are zero.

Overflow block owner is the inode identity. Payload: next u64 at 64, count u32
at 72, reserved u32 at 76, followed by count 24-byte extents at 80. Maximum 167
extents per block. Used payload=16+24*count; count must be positive. All blocks
except the last are full; exactly the declared number of extents must be present.
Maximum chain length=ceil((65536-4)/167)=393. Cycles, shared blocks, owner or
identity mismatches, excess/short chains, and noncanonical packing are rejected.

## Directories and filenames

Directory metadata blocks have their directory's owner identity. Records fill
exactly the used payload. Each record: record length u16 at 0, UTF-8 byte length
u16 at 2, reserved u32 at 4, child index u64 at 8, child generation u64 at 16,
name bytes at 24, then zero padding to a multiple of 8. Record length must equal
round_up(24+name_length,8); records cannot cross blocks. Empty blocks use zero
payload. No on-disk dot entries. Parent references and the complete linked tree
must agree; cycles and unreachable linked inodes are rejected.

Policy 1 is Unicode 16.0 canonical caseless matching:
`NFD(full_case_fold(NFD(name)))`. Dependencies are exactly caseless 0.2.2 and
unicode-normalization 0.1.24; Unicode versions are tested. See the upstream
[matching algorithm](https://docs.rs/caseless/0.2.2/src/caseless/lib.rs.html) and
[normalization tables](https://raw.githubusercontent.com/unicode-rs/unicode-normalization/v0.1.24/src/tables.rs).
Comparison keys are bounded to 16384 UTF-8 bytes. Exhaustive scalar expansion
checks establish that accepted 255-byte inputs cannot exceed this bound.
Equivalent keys within a directory are corruption. Accepted spelling is retained.

Reject invalid UTF-8, zero or more than 255 UTF-8 bytes, `.` and `..`, Unicode
Cc controls (U+0000..001F and U+007F..009F), any of `<>:"/\|?*`, and trailing
ASCII space or dot. Reject ASCII-case-insensitive DOS stems before the first
ASCII dot: CON, PRN, AUX, NUL, CLOCK$, CONIN$, CONOUT$, COM1..COM9, LPT1..LPT9,
and COM/LPT with superscript ¹,²,³. Strip trailing ASCII spaces/dots from the
stem for this test (thus `CON .txt` is also rejected). No compatibility folding
or locale-dependent transformations are used.

## Journal and recovery

Control payload: sequence u64 at 64 (positive), state u32 at 72 (0 clean,
1 committed), image count u32 at 76 (clean=0, committed=1..256), transaction
CRC32C u32 at 80 (clean=0), reserved u32 at 84. Used payload=24.
Two same-sequence controls must have identical payload. A clean-to-commit or
commit-to-clean transition increments sequence by one; exhaustion of u64
requires refusing further mutation (never wrapping). Read-only opening may
accept maximum sequence. Initial controls are both clean sequence 1.

Selection table (invalid means torn/CRC/encoding failure):

| Control pair | Selection |
| --- | --- |
| both invalid | reject |
| one valid | select valid, diagnose degraded control redundancy |
| both valid, same sequence, identical payload | select either |
| both valid, same sequence, differing payload | reject |
| differing sequence, difference != 1 | reject |
| differing sequence, same state | reject |
| differing by 1, opposite state | select newer |

Unsupported checksum-valid revisions are always errors. Selected clean means
no overlay. Selected committed requires validation of the ENTIRE payload first;
invalid payload rejects even if home blocks appear checkpointed.

For image i=0..count, descriptor is physical block 3+2*i; image is 4+2*i.
Descriptor payload: target u64 at 64, ordinal u32 at 72, image CRC32C u32 at 76,
sequence u64 at 80. Used payload=24; descriptor owner=(0,0). Image CRC covers
all image bytes including the image's own header checksum. Transaction CRC32C
covers concatenated descriptor,image pairs in ordinal order, including all
headers/checksums/padding. Targets must be distinct bitmap/table/directory/extent
metadata blocks; superblocks, controls, payload, raw file data and out-of-range
targets are forbidden. Type, physical home identity, owner and contextual
ownership must agree with the recovered view. All ordinary metadata, including
bitmap/table, is read through the bounded (256-block) overlay.

Writer ordering required by the storage contract (future engine): durably write
payload before publishing committed controls; do not checkpoint until both
committed controls are durable; durably checkpoint all home images before
publishing clean controls; do not reuse payload until both clean controls are
durable. Each individual control update is flushed. Therefore a torn control
can safely fall back to its valid peer. Readers never write, flush, checkpoint,
retire or reclaim. Complete recovered-view ownership validation precedes access.

Examples: clean seq1/seq1 reads home. Committed seq2/seq2 overlays every target.
Partially checkpointed seq2/seq2 still overlays every target (idempotent).
Interrupted retirement clean seq3/committed seq2 selects clean, because all home
images were flushed first. Torn clean control plus committed seq2 selects the
old payload, which remains intact until both clean controls are durable. Torn
commit plus clean seq1 selects home, because checkpointing has not started.
