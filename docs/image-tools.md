# Image creation and forensic inspection

```
cargo run -p xffs-tools --bin mkfs-xffs -- empty.img --size-mib 16 --uuid 58464653-0000-0001-8000-000000000001
cargo run -p xffs-tools --bin xffs-image -- create-demo demo.img --scenario committed
cargo run -p xffs-tools --bin xffs-inspect -- demo.img
```

Creators use create-new semantics and refuse existing paths. Geometry and demo
content are prepared before creation; file size belongs to the creator, never
ImageDevice. New images are zero-filled by the host filesystem (possibly sparse),
all referenced metadata/data is initialized, and sync_all precedes success.
An incomplete newly created image is removed after a write/flush failure.

The reproducible 64 MiB demo includes ReadMe.txt, Café.txt (lookup with decomposed
or upper-case spelling), run.sh, nested/binary.bin (bytes 0..255 repeated), an
empty nested directory, empty.txt, many/ with 160 empty files, overflow.bin (five
one-block extents filled with 0x5a), and sparse.bin. The sparse file has size
4294971392 with 0x11 in its first block and 0x77 at offset 4294967296; the rest
is a hole. Timestamps are fixed at Unix 1700000000, UUID is fixed, and allocation
order is deterministic. These are synthetic fixtures, not an importer or a
transaction engine.

Clean has Recovered.txt. Committed has Before.txt in its home directory and an
older root timestamp; a two-image transaction changes the name and inode table.
Partial-checkpoint has already copied the directory image home but still needs
the inode-table overlay. Their recovered state is identical to the clean image.

The inspector explicitly prints the raw HOME view and labels journal descriptors
as JOURNAL PAYLOAD. It is a forensic tool, not a declaration that an unrecovered
namespace is mountable. Recovered validation is provided by xffs-check.
Full images are Git-ignored. Tests compare generated files byte for byte.

Cargo/rustc reject a dot in a binary target name. The Cargo target is therefore
`mkfs-xffs`; `scripts/mkfs.xffs` supplies the conventional dotted command name.

The inspector includes bitmap allocation counts and journal image headers and
directory records. Its accumulated report is bounded to approximately 8 MiB
(one final bounded record may cross the threshold); larger inspections return a
resource-limit error. This prevents image contents from demanding unbounded
report allocation.
