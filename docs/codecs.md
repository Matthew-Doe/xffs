# Checked format codecs

`xffs_core::format` implements the [format specification](on-disk-format.md).
Decoders take bounded slices, validate whole-block CRC32C, identity, padding and
field limits, and return `FsError` separately from backend `DeviceError`.
`resolve_extents` enforces canonical chain packing, bounds and mappings. Its
caller must additionally verify allocation and global ownership. No codec
trusts a disk count to allocate more than one block or 65536 extent records.

`xffs_core::names` preserves spelling and produces bounded canonical caseless
keys. Tests pin both dependencies' Unicode version and exhaustively bound UTF-8
expansion across all Unicode scalar values. Golden controls are independently
constructed; tests check every byte's checksum coverage, truncation, malformed
records, illegal chain references, layout arithmetic and journal selection.
