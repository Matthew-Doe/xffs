# XFFS design decisions

This is the persistent record of the design interview. User answers are recorded
as decisions with rationale, consequences, and any required changes to
`filesystem-design-and-development-plan.md`. Recommendations and unanswered
questions are not user decisions. Later answers can supersede earlier decisions;
keep the history explicit.

## All decisions are mutable

The user explicitly states: "these decisions are mutable". Every decision below
is a current working choice, not an irreversible commitment. Revisit it when
experiments, implementation experience, or user preference warrant a change.
Record superseded choices and update the design together. A clear user revision
is sufficient; ask for clarification only when the intended change is ambiguous.
This applies to scope, language, guarantees, and mechanisms, not just parameters.
Mutability does not permit silently changing a supported image's interpretation
or weakening a published durability contract; revision/version changes must be
explicit.

## Confirmed project constraints

- **C-001 — Hobby project.** The user explicitly describes this as a hobby.
  Favor interesting, understandable work and achievable milestones; production
  adoption is not a prerequisite for success.
- **C-002 — Motivation.** Build a filesystem for flash drives that addresses
  complaints about FAT32 and exFAT. D-001 prioritizes understandable crash
  recovery over performance and breadth of interoperability for v0.0.1.
- **C-003 — Repository.** Initialize new coding projects as Git repositories.
  This workspace has been initialized.

## Status of the existing proposal

The design document is an assistant-authored proposal requested by the user.
Its specific choices—including Rust, metadata redo journaling, fixed inode
tables, synchronous operations, and case-sensitive filenames—are not treated as
individually accepted merely because the user requested the document.
Subsequent decisions below explicitly accept or supersede those proposals;
D-009 supersedes the original case-sensitive filename proposal.

## Round 1 — Purpose, durability, and freedom to redesign

### Q-001 — Primary success criterion

**Status:** decided — D-001.

**User answer:** "Crash recovery you can understand and demonstrate".

**Decision:** prioritize understandable, demonstrable crash recovery for v0.0.1
when it conflicts with performance optimization or breadth of interoperability.
This does not yet select journaling over COW or define data atomicity.

**Rationale:** the user selected this as the primary success criterion; no further
reason was supplied.

**Consequences:** recovery traces, a fault-injecting backend, and explicit
operation guarantees are central deliverables. Performance and additional OS
adapters remain secondary. Align the proposal's primary objective accordingly.

If v0.0.1 can excel at only one thing, which makes the project worth building?

- Crash recovery that is understandable and demonstrable.
- Flash performance and write efficiency.
- Convenient file exchange between operating systems.

**Why it matters:** sets priorities when simplicity, performance, and host
compatibility conflict. No ranking has been inferred from the original prompt.

### Q-002 — Interrupted file-content overwrite

**Status:** decided — D-002.

**User answer:** "1C", selecting the clarified option: ordinary writes can be
interrupted, but provide a reliable whole-file replacement workflow that prepares
a new file and then atomically replaces the old one.

**Decision:** support atomic publication of a completely staged replacement;
do not require arbitrary in-place writes to be all-or-nothing. Successful fsync
must still establish durability under the device contract.

**Rationale:** the user selected this tradeoff; no additional reason was supplied.

**Consequences:** specify and test staged write, file sync, atomic rename, and
parent-directory sync. The assistant's proposed implementation uses ordinary
temporary-file-plus-rename operations, without a new on-disk operation. This
implementation detail remains a proposal, not an independently selected user
preference. Space for both versions is required until the old one can be reclaimed.

When an existing file is overwritten and the drive is removed before completion,
what is the required result?

- Mixed old/new contents are acceptable for an interrupted write; successful
  fsync must establish durability under the device contract.
- Every individual write must recover entirely before or entirely after.
- Whole-file replacement must be atomic through an explicit replace operation.

**Why it matters:** metadata consistency, per-write data atomicity, and atomic
file publication are different guarantees. The current proposal offers the first,
not general atomic data overwrites. An explicit replacement workflow needs its
own definition of staging, fsync, publication, and reclamation.

**Remaining detail:** choose whether to expose a convenience tool/API in addition
to documenting the ordinary temporary-file-plus-rename workflow. The guarantee
does not transparently change how arbitrary applications overwrite files.

### Q-003 — Format evolution and COW

**Status:** decided — D-003.

**User answer:** "2A", selecting experimental format breaks and reformatting.

**Decision:** permit incompatible experimental disk-format revisions and require
reformatting when necessary. Do not require snapshot-ready architecture now.

**Rationale:** the user selected this tradeoff; no additional reason was supplied.

**Consequences:** document revisions and reject unsupported images rather than
silently interpreting them. Migration tools are not required for experimental
revisions. COW/snapshots remain possible later; this is not a permanent decision
against them or an affirmative selection of journaling.

How much later redesign is acceptable?

- Experimental format breaks and reformatting are acceptable.
- Plan for COW and snapshots now, accepting a larger initial scope.
- Commit to journaling; snapshots are not a project goal.

**Why it matters:** snapshots and COW affect allocation ownership, metadata
lifetimes, and commit protocols. Reserved fields do not supply those semantics.

## Round 2 — Durability cost, integrity, and actual workloads

### Q-004 — When to pay for durability

**Status:** decided — D-004.

**User answer:** "3A".

**Decision:** each mutating core operation waits for durable completion in the
initial version. Accept the cost of synchronous persistence rather than batching
acknowledged operations and requiring a later sync to make them durable.

**Rationale:** the user selected this tradeoff; no additional reason was supplied.

**Consequences:** retain the synchronous transaction protocol, propagate flush
errors, and test acknowledgment boundaries. Application/kernel buffering before
requests reach the core is outside this acknowledgment guarantee. Large writes
can still complete in chunks and return partial progress, as documented.

Should each mutating core operation wait for durable completion, or should the
core batch transactions and require explicit fsync/safe eject for durability?
The first is the existing proposal and the recommended starting point for an
explainable recovery prototype; it may be slow for many small files. The second
adds pending-state and acknowledgment rules. Application and kernel buffering
must not be confused with an operation already delivered to the core.

### Q-005 — Silent file-content corruption

**Status:** decided — D-005.

**User answer:** "4A".

**Decision:** metadata checksums are sufficient for v0.0.1; defer file-content
checksums. Do not promise detection of silent corruption in user data.

**Rationale:** the user selected this scope; no additional reason was supplied.

**Consequences:** retain the existing metadata-only integrity scope and clearly
distinguish crash consistency from file-content integrity. End-to-end comparison
in tests verifies implementation behavior but is not an on-disk checksum feature.

Are metadata checksums enough for v0.0.1, or must it also detect corruption in file
contents? Data checksums add checksum/data ordering decisions for interrupted
in-place overwrites. Detection alone does not repair the original bytes.

### Q-006 — Representative workload

**Status:** decided — D-006.

**User answer:** "5C, 128gb mixed use".

**Decision:** target a roughly 128 GB flash drive with a general-purpose mixture
of large and small files. Interpret "mixed use" as including both copying files
and editing files directly; no particular ratio or speed requirement was given.

**Consequences:** include bulk copies, small-file directory workloads, in-place
edits, atomic replacement, and near-full-volume behavior in demonstrations and
performance measurements. This is the reference workload, not a maximum volume
size or a restriction on file types. Use decimal 128 GB for illustrative sizing;
actual formatting always uses reported backend capacity.

What should the first real drive mostly store: large media/images, many small
source/document files, or a mixed general-purpose collection? Also ask typical
drive capacity and whether mostly copying new files or editing existing ones.
Use the answer to select demonstrations and performance workloads, not to infer
unsupported restrictions on other file types.

## Round 3 — Persistence mechanism and everyday format limits

### Q-007 — Journal or COW for the first implementation?

**Status:** decided — D-007. User-facing question 6.

**User answer:** "6A".

**Decision:** use the proposed metadata redo journal for the first implementation.

**Consequences:** retain the commit/replay/checkpoint development path and its
repeated metadata writes. COW remains deferred, not permanently prohibited.
The exact journal sizes and wire encodings remain implementation proposals.

- A: keep the proposed metadata redo journal, accepting repeated metadata writes
  in exchange for one explicit commit/replay path.
- B: replace it with metadata COW and a durable root-switch protocol, accepting
  allocation/reclamation redesign. This does not automatically include snapshots
  or atomic in-place user-data writes.

**Recommendation:** A for the current prototype because the proposal and
development order already describe its recovery protocol. COW remains a valid
alternative, not inherently a worse or universally more complex design.

### Q-008 — Fixed inode capacity or growth?

**Status:** decided — D-008. User-facing question 7.

**User answer:** "7A".

**Decision:** use a fixed inode table with capacity selected at formatting time.
Accept reserved table space and possible inode exhaustion despite free data space.

**Consequences:** expose inode capacity in mkfs/statfs and test exhaustion. The
illustrative density below remains a suggested default, not a separately chosen
requirement.

- A: fixed, formatter-selected inode capacity; accept exhaustion while data space
  remains, and reserve the table space up front.
- B: grow inode storage as needed; add a mapping/allocation scheme and its crash
  consistency rules.

**Sizing illustration:** decimal 128 GB at one inode per 64 KiB yields about
1.95 million slots. At 15 records per 4 KiB block, the table uses about 533 MB
(508.6 MiB), approximately 0.42% of the volume, including headers and padding.
These are proposal-derived estimates, not a selected final inode density.

### Q-009 — Case-sensitive names or portable case-insensitive lookup?

**Status:** decided — D-009. User-facing question 8.

**User answer:** "8B".

**Decision:** use case-insensitive, case-preserving filenames from v0.0.1.
`Report.txt` and `report.txt` identify the same directory name and cannot coexist.

**Consequences:** supersede the case-sensitive proposal. Pin Unicode comparison
behavior in the format, retain original spelling, reject equivalent duplicate
names, and support case-only rename. Normalization and full versus simple folding
were left open here and are subsequently settled by D-010. Native host behavior
must not silently decide on-disk identity.

**Rationale for D-007 through D-009:** the user selected these options; no further
reasons were supplied.

- A: case-sensitive UTF-8 byte comparison initially; allow `Report.txt` and
  `report.txt` as distinct names and accept later case-collision export handling.
- B: case-insensitive, case-preserving lookup initially; define deterministic,
  versioned Unicode comparison rules and reject equivalent names.

**Recommendation:** A for a smaller first implementation. Either choice still
needs a separate host-reserved-name policy for cross-platform adapters.

## Round 4 — Unicode and portability

### Q-010 — Which Unicode equivalences count as the same filename?

**Status:** decided — D-010. User-facing question 9.

**User answer:** "9A".

**Decision:** full Unicode case folding with canonical normalization. Preserve
original stored spelling while comparing canonical caseless keys. `Straße.txt`
and `STRASSE.txt` collide, as do canonically equivalent accented spellings.

**Consequences:** pin the algorithm and Unicode data version in the format; apply
one rule consistently to lookup, create, rename, duplicate detection, and checker
validation. This selects canonical equivalence, not arbitrary visual similarity,
accent stripping, locale-specific matching, or compatibility normalization.
The precise Unicode release remains an implementation choice to freeze before
writing images.

- A: full Unicode case folding plus canonical normalization; e.g. `Straße.txt`
  and `STRASSE.txt` collide, as do composed/decomposed forms of accented letters.
- B: simple Unicode case folding plus canonical normalization; retain the
  distinction between `Straße.txt` and `STRASSE.txt`, while still equating canonical
  encodings of accented letters and simple case variants.

Both proposed options preserve the original stored spelling, use locale-neutral
rules pinned to a specified Unicode version, and do not promise compatibility
with any host filesystem's exact comparison rules. The recommendation was A for
adopting Unicode canonical caseless matching as a specified algorithm; the user
selected A, including canonical normalization.

Reference: [Unicode 16.0 core specification, chapter 3](https://www.unicode.org/versions/Unicode16.0.0/core-spec/chapter-3/).

### Q-011 — First-release operating systems

**Status:** decided — D-011. User-facing question 10.

**User answer:** "10A".

**Decision:** Linux FUSE is the only required mount adapter for v0.0.1.

**Consequences:** Windows/macOS adapters remain later work. Keep the core portable
and comparison rules independent of the host; do not make multi-OS mounting a
release gate.

- A: Linux FUSE only for v0.0.1; other adapters later.
- B: Linux and Windows read/write mounting required for v0.0.1.
- C: Linux, Windows, and macOS mounting required for v0.0.1.

Recommend A for an earlier recovery milestone. Choosing case-insensitive names
does not itself authorize or require additional adapters.

### Q-012 — Portable filename restrictions

**Status:** decided — D-012. User-facing question 11.

**User answer:** "11A".

**Decision:** enforce conservative portable filename restrictions in the core
from v0.0.1, including on Linux.

**Consequences:** specify a single rejection policy for creation and rename;
do not silently sanitize names. Exact forbidden characters, device-style names,
suffixes, and length rules remain to be specified and reviewed. This does not
promise universal compatibility with every host or application.

**Rationale for D-010 through D-012:** the user selected these options; no further
reasons were supplied.

- A: enforce a documented conservative portable-name policy in the core now,
  rejecting host-problematic names even on Linux.
- B: retain broad UTF-8 name acceptance; later adapters/export tools must reject
  or explicitly translate names a destination cannot represent.

Recommend A if routine file exchange across operating systems is important. This
is not a claim that one restriction list guarantees all forms of portability.
The exact forbidden characters, names, suffixes, and length policy need a written
specification if A is chosen; no current directory entries are silently renamed.

## Round 5 — Corruption behavior, permissions, and language

### Q-013 — What happens when metadata corruption is detected?

**Status:** decided — D-013. User-facing question 12.

**User answer:** "12B".

**Decision:** attempt best-effort read-only access to validated files after
metadata corruption, with explicit errors for damaged regions. Stop writes; do
not automatically repair.

**Consequences:** define a degraded read-only mode distinct from healthy read-only
mounting. Validate the metadata dependencies of anything exposed. A valid block
checksum alone is insufficient if pointers, ownership, or journal state are
uncertain. Refuse access when a trustworthy view cannot be established; do not
promise that every corrupted volume remains mountable. Data integrity remains
outside the guarantee because D-005 defers file-content checksums.

- A: refuse normal mounting for detected corruption, or stop normal service if
  discovered while mounted; use explicit read-only inspection/salvage tooling.
- B: attempt a best-effort read-only mount that exposes independently validated
  files and returns explicit errors for damaged regions.

Both options stop writes and avoid automatic repair. Option B requires a precise
definition of what remains trustworthy; corrupt allocation or namespace metadata
can affect more than the block where damage was found. Recommend A initially.
Detection policy does not imply a full pre-mount scan; eager versus lazy checking
is a separate implementation decision. Known recoverable journal states and a
single damaged superblock with a valid twin follow their explicit recovery rules.

### Q-014 — Persistent permission metadata

**Status:** decided — D-014. User-facing question 13.

**User answer:** "13B".

**Decision:** persist an executable flag for regular files while synthesizing
ownership and other permissions from mount options. Full POSIX ownership/modes
and ACLs remain deferred.

**Consequences:** include the flag in inode encoding and metadata transactions;
define create/chmod/stat mappings and honor noexec policy. Do not claim to
preserve separate owner/group/other execute permissions with one bit. Exact
mapping and default mount policy remain implementation details to specify.

- A: no persistent per-file permissions initially; synthesize ownership and modes
  from mount options, accepting that executable bits are not preserved.
- B: preserve an executable flag for regular files, but synthesize ownership and
  remaining permissions; define how chmod and non-executable mounts interact.
- C: store POSIX ownership and modes, accepting identity mapping and enforcement
  questions when moving drives between machines. ACLs remain separately deferred.

Recommend B if the mixed workload includes scripts/source trees, otherwise A for
a smaller first implementation. None of these permissions substitutes for
encryption or protects against someone with unrestricted raw-device access.

### Q-015 — Implementation language

**Status:** decided — D-015. User-facing question 14.

**User answer:** "Rust".

**Decision:** implement the core, tools, and initial adapter in Rust.

**Consequences:** retain explicit byte encoding independent of native struct
layout. Dependency choices and boundaries for any unsafe code are not yet
selected. Rust does not replace crash tests or guarantee logical correctness.

**Rationale for D-013 through D-015:** the user selected these options; no further
reasons were supplied. All remain mutable under the policy above.

- A: Rust, as currently proposed.
- B: C.
- C: another language selected by the user.

Recommend A for explicit binary parsing with memory-safe code. The user's
familiarity and learning interests matter for a hobby project; the user selected
Rust. Disk encodings remain independent of language-native layouts.

## Round 6 — Validation timing and dependency scope

### Q-016 — Full checking before normal writable mounts?

**Status:** decided — D-016. User-facing question 15.

**User answer:** "15A".

**Decision:** run the full metadata and ownership checker before admitting a
normal writable mount, accepting mount-time cost.

**Consequences:** validate the logical post-recovery view using the journal
overlay before admitting normal mutations. Recognize legitimate orphan and
pending-reclamation states; damage routes to the degraded read-only policy.
Continue validation during access, since a mount-time scan cannot prevent later
damage. This does not add file-content checksums or a data scrub.

- A: run the full metadata/ownership checker before admitting normal writable
  service; accept mount-time cost to catch existing structural damage early.
- B: validate bootstrap/journal state on mount and validate other metadata on
  access, leaving full ownership checking to an explicit checker.

Recommend A for the recovery-focused prototype. With the proposed 128 GB inode
density, scanning the inode table alone reads roughly 533 MB, before other
metadata. This is an estimate, not a measured mount time. B detects some damage
later and needs careful allocator safety assumptions. Neither verifies user-data
integrity. Degraded read-only access still needs explicit trust validation.

### Q-017 — Libraries versus hand-written support code

**Status:** decided — D-017. User-facing question 16.

**User answer:** "16A".

**Decision:** use existing Rust libraries for Unicode, checksums, FUSE, and CLI
parsing; implement the filesystem structures, allocator, journal, and recovery.

**Consequences:** evaluate and pin concrete dependencies during scaffolding. Keep
library Unicode behavior consistent with the format's pinned comparison policy.
Use format test vectors to check encoding/checksum behavior independently of
library selection. No specific crate or version has been approved or selected.

**Rationale for D-016 and D-017:** the user selected these tradeoffs; no further
reasons were supplied. Both are mutable like the other working decisions.

- A: use existing Rust crates for Unicode, checksums, FUSE, and command-line
  parsing; write filesystem structures, allocation, transactions, and recovery.
- B: minimize dependencies and implement more support code for learning; identify
  which components the user actually wants to build before committing the scope.

Recommend A to concentrate effort on the filesystem. Pin dependencies and Unicode
data behavior; adopting libraries does not delegate the on-disk specification.

## Round 7 — Performance expectations, reclamation latency, and execution

### Q-018 — Performance as a first-release gate

**Status:** decided — D-018. User-facing question 17.

**User answer:** "17A".

**Decision:** measure and report performance without a numeric speed gate for
v0.0.1. Correct recovery remains the release priority.

**Consequences:** retain representative performance workloads and publish results;
do not preemptively optimize away the selected synchronous/checking behavior.
Hangs, resource-exhaustion defects, and unbounded algorithms are still defects.

- A: measure and report performance, but do not require a numeric speed target
  for v0.0.1; correct recovery is the release gate.
- B: set user-specified mount/copy/small-file latency limits before proceeding,
  and revisit current mechanisms if measurements miss them.

Recommend A until there are real measurements on the reference drive. This does
not excuse hangs, unbounded resource use, or unusable algorithmic defects.

### Q-019 — When should deleted/truncated space be reclaimed?

**Status:** decided — D-019. User-facing question 18.

**User answer:** "18A".

**Decision:** complete eligible reclamation synchronously before returning
success, using bounded journal transactions. No deferred cleanup work queue is
required for v0.0.1.

**Consequences:** deletion, truncation, and replacement can have substantial
latency. Open unlinked/replaced files retain their storage until final close.
After a crash, mount recovery finishes interrupted cleanup before writable
service. An error during cleanup can follow an already durable logical change;
do not promise rollback of the namespace or size in that case.

- A: synchronously finish eligible bounded reclamation before returning success;
  accept latency for a simpler foreground state machine.
- B: return after the namespace/size change and reclaim marker are durable,
  processing physical reclamation later through a defined work queue.

Both preserve synchronous durability of acknowledged logical changes (D-004).
B does not free blocks early; it postpones freeing them and requires scheduling,
space-pressure, unmount, and recovery rules. Open unlinked files retain their
storage until last close in either design. Recommend A initially, consistent with
the original proposal; the user subsequently selected A.

### Q-020 — Default execution policy

**Status:** decided — D-020. User-facing question 19.

**User answer:** "19A".

**Decision:** honor the persistent executable flag by default, constrained by
mount modes, and provide an explicit noexec option.

**Consequences:** document and test both mount policies without changing the
stored flag. The exact single-flag-to-mode mapping still needs specification.

**Rationale for D-018 through D-020:** the user selected these options; no further
reasons were supplied. All decisions remain mutable.

- A: honor the persistent executable flag by default, constrained by mount modes;
  allow an explicit noexec mount.
- B: default mounts to noexec, preserving the flag but requiring an explicit exec
  option to permit direct execution.

Recommend A for a drive used for scripts/source files, with B as a preference
choice. This governs direct execution policy, not a guarantee that stored code
cannot be read and run through an interpreter.

## Current working baseline

The initial interview has no unanswered questions. These are the current choices;
the rationale/history above and the mutability policy remain authoritative.

| Area | Current choice |
|---|---|
| Primary goal | Understandable, demonstrable crash recovery. |
| Reference use | Roughly 128 GB, mixed files and usage. |
| Initial platform/language | Linux FUSE, Rust, existing support libraries. |
| Commit design | Synchronous metadata redo journal; ordinary overwrites need not be atomic. |
| Safe replacement | Stage and sync a new file, then atomically publish it. |
| Integrity | Metadata checksums; file-content checksums deferred. |
| Writable mount | Full metadata and ownership validation first. |
| Corruption | Attempt degraded read-only access to validated files; explicit errors elsewhere. |
| Inodes | Fixed table sized at formatting time. |
| Names | Case-preserving, full Unicode canonical caseless comparison, portable-name restrictions. |
| Permissions | Persistent executable flag, other modes/ownership synthesized; exec honored by default with noexec available. |
| Reclamation | Synchronous when eligible; preserve open-file lifetime. |
| Performance | Measure/report; no numeric first-release gate. |
| Evolution | All decisions mutable; experimental format breaks and reformatting acceptable. |

## Remaining engineering details, not unanswered interview questions

Specify these during format design and implementation; distinguish proposed
defaults from explicit user choices:

1. Exact portable-name restrictions and pinned Unicode release/algorithm vectors.
2. Executable flag mappings for create/chmod/getattr and mount masks.
3. Exact binary layouts, checksum vectors, numeric bounds, and formatter defaults.
4. Concrete Rust dependencies and supported toolchain versions.
5. Degraded-mode trust validation, diagnostic presentation, and cache transitions.
6. Whether a convenience replacement tool is useful beyond documented file/sync/
   rename operations.

Resume the interview when an implementation tradeoff changes a user-visible
guarantee or the user wants to revisit a choice; no additional generic preference
round is necessary to start the agreed development sequence.

## Recording an answer

Each resolved question records the user's answer, its interpreted scope,
rationale when supplied, consequences, and whether the design proposal needs
updating. Do not invent a rationale. If an answer clearly revises an earlier
choice, record the supersession; ask only when the intended change is ambiguous.
