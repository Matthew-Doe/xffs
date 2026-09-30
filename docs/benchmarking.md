# Filesystem comparisons for XFFS 0.0.2

`scripts/benchmark-filesystems.py` compares prebuilt XFFS with exFAT, FAT32
and ext4 on Linux. F2FS and an independently supplied XFFS 0.0.1 build are
optional. This harness does not change the disk format, optimize XFFS, build
binaries, install dependencies, fetch releases, or perform physical-unplug tests.
The serial-bound [USB acceptance harness](usb-acceptance.md) remains separate.

## Disposable images

Run through sudo from an ordinary account, using a **new** output directory:

```sh
sudo python3 scripts/benchmark-filesystems.py image --output /var/tmp/xffs-comparison-001
```

The default matrix is `xffs exfat fat32 ext4`, three repetitions, 512 MiB per
image, using `target/release/mkfs-xffs` and `target/release/mount-xffs`.
Each sample gets a new equally sized sparse image. XFFS is formatted through
its image interface, then attached to a loop device for mounting; the kernel
competitors are also formatted in their image files and mounted through loop
devices. Images are retained as evidence, so budget host space for the entire
matrix. These are **host-backed image measurements**, including the host
filesystem, sparse allocation, cache and storage effects, not USB benchmarks.

Select competitors and prebuilt binaries explicitly:

```sh
sudo python3 scripts/benchmark-filesystems.py image \
  --filesystems xffs exfat fat32 ext4 f2fs \
  --bin-dir /absolute/path/current/target/release \
  --baseline-bin-dir /absolute/path/0.0.1/target/release \
  --repetitions 3 --image-mib 512 --timeout 300 \
  --output /var/tmp/xffs-comparison-002
```

Supplying `--baseline-bin-dir` adds `xffs-0.0.1` to the matrix. It must be an
independent, compatible build; its label is user-supplied provenance, not a
version inferred from the binary. Required CLI flags are checked before any
format. Binary SHA-256 hashes, help/version output, and available Git HEAD and
working-tree status near each executable are recorded. A nearby checkout does
not prove a binary was built from that checkout. Missing or incompatible
explicit baselines are never silently skipped.

## Physical USB measurements

**Every selected filesystem and repetition erases the entire named disk.**
`--erase` authorizes all formats in the printed run matrix, including optional
competitors. There is no resume or automatic formatter retry.

```sh
sudo python3 scripts/benchmark-filesystems.py device \
  --device /dev/disk/by-id/usb-YOUR_WHOLE_DISK \
  --expect-serial YOUR_EXACT_SERIAL --erase \
  --output /var/tmp/xffs-usb-comparison-001
```

The disk must be a whole removable USB disk with the matching serial. Before
formatting and mounting, the runner rechecks capacity, logical sector size,
disk sequence, path, USB identity and topology. It refuses mounted disks or
children, swap, active holders and evidence on dependent target storage. Host
evidence dependencies are traced through partitions, stacked devices and loop
backing files. For physical runs, evidence on FUSE, overlay, Btrfs or other
storage whose dependencies cannot be established is refused; choose a separate
ordinary block-backed host filesystem or tmpfs. Keep the device attached and
prevent other programs from automounting or using it throughout the run. Checks
cannot prevent an unrelated privileged process from racing device management.

Competitors all use whole-device filesystems. Old signatures are removed only
inside the authorized formatting phase; kernel partitions are refreshed and
checked before proceeding. No lazy unmount is used. Failed or interrupted runs
stop the matrix and retain evidence. A cleanup failure requires manual
inspection of the logged mountpoint/device; never launch another run over an
incompletely torn-down run.

## Prerequisites and exit status

Mounting and loop management require Linux root with `CAP_SYS_ADMIN`. Workloads
and verification run as the non-root `SUDO_UID`/`SUDO_GID`, with supplementary
groups cleared. XFFS mounts claim the device as root and drop privileges using
the mount tool's existing device interface.

Common tools: Python 3, `lsblk`, `mount`, `umount`; image mode also needs
`losetup` and usable loop devices. Device mode needs `wipefs`, `blockdev`, and
`udevadm`. XFFS requires `/dev/fuse`, `fusermount3` and both prebuilt executables.
Each kernel filesystem requires its formatter and either loaded kernel support
or an available module (checked with `modprobe -n`; mounting may auto-load it).
The runner does not install anything. exFAT tools must support `--no-discard`
and `--partition-table`; FAT tools must support `--mbr`. Older incompatible
formatter versions are reported as missing prerequisites before formatting.

Exit 0 means the selected comparison completed and verified with cleanup.
Exit 77 means unmet prerequisites, **not a passing benchmark**. Invalid options,
failed commands, unsupported synchronization, corrupt remount contents or
workload timeouts return nonzero; interruption normally returns 130. Command
logs and partial JSON are preserved once the report directory is created.

## Workloads and timing

All filesystems use the same non-root Python worker and deterministic content.
Payloads and chunk views are generated before timing; hashing and report writes
happen afterward. Short writes are completed in a loop; zero progress fails.
Measurements use the monotonic clock and ordinary buffered writes.

| Workload | Default | Timed interval |
|---|---|---|
| Bulk sequential (`bulk`) | 8 MiB, 128 KiB chunks | Create, writes, final file fsync, close |
| Durable sequential (`durable`) | 1 MiB, 4 KiB chunks | Create, write/fsync for each chunk, close |
| Small files (`small`) | 100 files, 4 KiB each | Create/write/fsync/close each; final parent fsync |
| Atomic replacement (`replace`) | 20 replacements, 64 KiB each | Create/write/fsync/close temporary file, rename over destination, parent fsync |

The existing replacement destination is prepared outside timing. Each workload
has its own directory; order is always bulk, durable, small, replace. Filesystem
order rotates left one position per repetition. Each filesystem/repetition
starts with a new format. The default per-workload deadline is 300 seconds;
`--timeout` changes it. A timeout is a failed sample with no reported throughput.

Sizes are integer bytes and all values must be positive. Options are
`--bulk-bytes`, `--bulk-chunk`, `--durable-bytes`, `--durable-chunk`,
`--small-count`, `--small-bytes`, `--replace-count`, and `--replace-bytes`.
FAT32 selection rejects individual files larger than 4 GiB minus one byte.
Choose workloads that fit both RAM (payloads are prepared in memory) and the
formatted filesystem; filesystem metadata reduces usable capacity.

Throughput and operations/second use the full timed interval. Sequential
operations count chunks; small-file operations count files; replacements count
renames. Per-operation latency is recorded with median and nearest-rank p95:

- Bulk latency measures each write call, excluding its final file fsync/close.
- Durable latency measures each write plus fsync, excluding create/close.
- Small-file latency includes create/write/fsync/close but excludes the final
  directory fsync, which remains included in total time.
- Replacement latency includes the entire replacement and directory fsync.

XFFS also synchronizes modifying operations internally. Equal application calls
therefore do not imply identical durability costs. The bulk and durable profiles
must be compared separately. Kernel filesystems keep their normal durability
settings; the runner neither forces synchronous mounts nor disables journaling.
Actual mount and superblock options are recorded. Unsupported fsync calls,
including directory fsync, mark the workload unsupported and the comparison
incomplete; they are never treated as successful no-ops.

Optional format-time discard is disabled for ext4, exFAT and F2FS. ext4 lazy
inode-table and journal initialization are disabled. FAT32 uses no synthetic
partition table, and exFAT explicitly uses `-P none`. Exact formatter arguments
are saved. Fresh formatting **does not reset flash-controller state**. No
cross-filesystem cache normalization, media preconditioning or claims of raw
media bandwidth are made.

## Verification and evidence

After all workloads the runner cleanly unmounts, remounts read-only, and checks
the exact file names, sizes, regular-file types and SHA-256 contents in each
workload directory against deterministic expectations. This verifies persistence
after a clean remount, not survival of power loss or physical unplugging.

The new report directory contains:

- `results.json`: versioned schema, matrix, device identity, kernel/tools,
  provenance, parameters, timings, capacities/free space, actual mount options,
  verification and cleanup outcomes, including missing samples.
- `parameters.json`, worker JSON files, `commands.jsonl` and numbered command
  logs, including exact formatter invocations and failed command output.
- `samples.csv` and `comparison.md`: explicit missing/failed/unsupported
  workloads and aggregates with median and min/max across repetitions.
- Retained image files for image mode.

Only measured and remount-verified workloads from successful samples with
complete teardown enter aggregates. Failed samples retain raw diagnostic
measurements in JSON but contribute no reported rates. A partial matrix stays
incomplete; it is not silently reduced to its successful competitors.

Regenerate CSV and Markdown without root or device access, into another new
directory:

```sh
python3 scripts/benchmark-filesystems.py report \
  /var/tmp/xffs-comparison-001/results.json --output /tmp/comparison-regenerated
```

## Tests

Ordinary tests are host-only and never select physical devices:

```sh
python3 -m unittest discover -s tests/hardware -v
```

Explicit disposable-image integration covers the four default filesystems with
reduced workloads, remount verification and clean teardown:

```sh
sudo python3 scripts/benchmark-image-smoke.py \
  --bin-dir "$PWD/target/release" --output /var/tmp/xffs-image-integration-001
```

This test never accepts a physical-device argument and never installs or builds
prerequisites. Exit 77 identifies a blocked integration check. USB performance
acceptance is a separate, explicitly invoked `device` run: all selected workloads
must complete, verify after remount, and leave the device unmounted. No privileged
CI setup or automatic physical-device execution is included.
