# XFFS

Experimental filesystem project focused on crash recovery that can be understood
and demonstrated. This is the **phases 0–1 storage milestone** for v0.0.1: a
byte-addressed storage API, bounded image-file backend, and deterministic crash
simulator. There is no disk format, formatter, journal, allocator, or FUSE mount
yet. Design decisions remain mutable; no image-format stability is promised.

- `crates/xffs-core`: storage contract, typed errors, and locked image-file I/O.
- `crates/xffs-sim`: volatile/durable state, explicit faults, fragment persistence,
  and diagnostic traces. Depends on core; core does not depend on simulation.
- [Storage contract](docs/storage-contract.md): implemented semantics and limits.
- Existing design records: [decisions](design-decisions.md),
  [development plan](filesystem-design-and-development-plan.md), and
  [original scope](filesystem-spec-pain-points-and-0.0.1.md).

Use an installed Rust toolchain **1.89 or newer** (edition 2024). Both crates use
only the standard library, forbid unsafe code, and are unpublished. No license
has been selected; release packaging is deferred. `Cargo.lock` is included.

```sh
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -p xffs-sim --example crash_demo
```

The example asserts and prints four outcomes with operation traces: an unflushed
write disappears, a flushed write survives, a torn write persists partly, and
overlapping writes persist in a selected order. It uses four bytes of simulated
storage and never opens a physical drive.

Tests share the same storage contract between backends, use uniquely created
temporary images, and exercise locks in a separate process. They need no root
access or physical flash drive. Host flush/reopen tests verify the image backend;
only the simulator supplies controlled power-loss evidence. Hardware durability
still depends on the host filesystem, OS, device, and their flush guarantees.
