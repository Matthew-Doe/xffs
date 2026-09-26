# New Flash-Drive Filesystem — Pain Points & v0.0.1 Scope

## Pain Points Being Solved

### Flash-aware core
- **Write amplification** — legacy in-place update formats (FAT/exFAT) don't consider flash wear; a log-structured/COW design fixes this.
- **No native TRIM/discard** — bolted on late in most formats; should be first-class from the start.
- **Fragmentation collapse** — FAT's linked-cluster chains get catastrophically slow on fragmented/large directories; extent-based allocation avoids this.
- **No wear-leveling awareness** — especially bad on cheap drives with minimal/no FTL.

### Reliability
- **Silent corruption** — FAT/exFAT have no data or metadata checksums; a yanked drive can corrupt silently with no detection.
- **No crash consistency** — no journaling or COW means "please run chkdsk" is the standard recovery path.
- **Single point of metadata failure** — no redundant superblocks.

### Compatibility & practicality
- **4GB file size limit** — still a real-world problem with FAT32.
- **Volume size ceilings** — arbitrary legacy limits.
- **No case sensitivity option** — inflexible for cross-platform use.
- **8.3 short-name legacy baggage** — wastes space and complexity for no modern benefit.
- **No sparse file support.**
- **No transparent compression option.**

### Cross-platform
- **Patent encumbrance** — exFAT's patent licensing kept it out of open-source OSes for years.
- **Implementation complexity** — overly complex formats don't get adopted by embedded devices (cameras, car stereos, game consoles); simplicity drives ubiquity, which is why FAT32 persists.

### Security/permissions
- **No optional permissions model** that degrades gracefully on systems that ignore it.
- **Vendor-locked encryption** — e.g., BitLocker-to-go isn't portable across ecosystems.

### Metadata
- **No extended attributes/tags.**
- **Coarse timestamps** — FAT's 2-second timestamp granularity is a long-running joke; no timezone info.

---

## What's in 0.0.1

**Goal:** prove the on-disk format works and is mountable/testable. Not production-safe yet.

### On-disk structure
- Single superblock at a fixed location (e.g., offset 1MB) + one backup copy at the end of the volume.
- Simple extent-based allocation (start block + length pairs) — no B-trees yet.
- Flat free-space bitmap (no tree/index yet).
- Fixed-size inode table **or** inline inodes in directory entries — pick one, not both.

### Must-have
- 64-bit file sizes and offsets (get this right now — never revisited later).
- UTF-8 filenames, no short-name fallback.
- Checksums on metadata blocks only (data checksums deferred).
- Basic timestamps: created + modified, nanosecond resolution, stored UTC + offset.
- Minimal journal: just enough to know if a write completed or not (no full ordered journaling yet).

### Explicitly deferred
- Compression
- Encryption
- ACLs / xattrs
- COW / snapshots
- Wear-leveling smarts (rely on the drive's FTL)
- Native Windows/macOS mounting (FUSE-based compat only)

### Included anyway, because retrofitting is painful
- Version field + feature-flags bitmap in the superblock, so future versions can add optional features without breaking old readers.
- Reserved/padding fields in every core structure for future expansion.

### Driver scope (0.0.1)
- Read-write FUSE driver (Linux first; macFUSE/WinFsp for cross-platform testing).
- Read-only path implemented and hardened first, then write support added.
- Single-threaded, single-mount — no concurrency handling yet.
- Core ops: mount/unmount, statfs, lookup, readdir, getattr, open, read, write, create, truncate, unlink, rename, mkdir/rmdir, fsync (naive flush is fine).
- Clean separation: `libyourfs` core library, separate from FUSE glue, talking to a single block-device abstraction layer.
- Write path strictly follows: allocate → write data → write metadata → journal commit.
- `--readonly` fallback mode for interop testing.
- Testing harness (fuzz mount/op/unmount/remount cycle) built *before* refining the on-disk format further.
