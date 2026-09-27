# XFFS

Experimental read-only filesystem for regular image files, with a deterministic
crash simulator and in-memory metadata journal recovery. The reader validates the
entire recovered namespace and allocation ownership before exposing it. There is
no format-stability promise, mounted writing, writable recovery, reclamation,
physical-device support, or host-directory importer.

- `xffs-core`: storage API, checked codecs, portable Unicode names, recovered reader.
- `xffs-sim`: deterministic crash/fault model and the retained crash demonstration.
- `xffs-tools`: formatter, demo generator, raw inspector, recovered checker.
- `xffs-fuse`: foreground Linux FUSE adapter using fuser without libfuse.

All workspace crates are unpublished, prohibit unsafe code, and require Rust
1.89 or newer. Dependencies are recorded in Cargo.lock; Unicode/CRC/FUSE versions
are pinned. No project license has been selected; release packaging is deferred.

```sh
cargo run -p xffs-tools --bin xffs-image -- create-demo demo.img --scenario committed
cargo run -p xffs-tools --bin xffs-check -- demo.img
mkdir -p mnt
cargo run -p xffs-fuse --bin mount-xffs -- demo.img mnt
# In another terminal:
ls -la mnt
find mnt
stat mnt/sparse.bin
cat mnt/Recovered.txt
fusermount3 -u mnt
```

Rust disallows dots in Cargo binary target names: use `mount-xffs`/`mkfs-xffs`
with Cargo, or `scripts/mount.xffs`/`scripts/mkfs.xffs` for conventional dotted
command names. The mount stays in the foreground, holds a shared image lock,
and uses `ro,nosuid,nodev,default_permissions`. Add `--noexec` to suppress direct
execution. Filename spelling is preserved; lookup uses Unicode 16.0 canonical
caseless matching. Image creation refuses existing paths.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -p xffs-sim --example crash_demo
# Explicit real mount test (Linux with /dev/fuse and fusermount3):
./scripts/mount-smoke.sh
```

Ordinary tests require no FUSE device. The real mount smoke test fails with
exit 77 and an **UNMET PREREQUISITE** message if `/dev/fuse` or a required tool
is missing; that result is not a pass. The real mount smoke test passed on this
Linux host on 2026-09-27, outside the agent sandbox. The sandbox hides
`/dev/fuse` even though it exists on the host; running this test from that
sandbox requires escalation. The smoke test has timeouts and unmount cleanup,
checks standard commands and write rejection, and compares entire images
before/after repeated clean and journal-recovered mounts.

See the [byte-level format](docs/on-disk-format.md),
[storage durability contract](docs/storage-contract.md),
[image tools and fixture contents](docs/image-tools.md),
[read-only API and validation](docs/read-only.md), and
[checked codecs](docs/codecs.md). Historical design records remain in
[design-decisions.md](design-decisions.md),
[filesystem-design-and-development-plan.md](filesystem-design-and-development-plan.md),
and [filesystem-spec-pain-points-and-0.0.1.md](filesystem-spec-pain-points-and-0.0.1.md).
