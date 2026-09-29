# Explicit USB acceptance

Target: whole removable USB disk with serial **0085199340190280**, observed size
**124,623,257,600 bytes** (116.1 GiB). Device paths are rediscovered on every
invocation/reconnect. The harness rejects other serials, non-USB/non-removable
devices, and capacity outside 115–117 GiB. The backend additionally claims the
disk and refuses mounted targets/children, swap, holders and system filesystems.
The harness passes `--expect-serial` and `--expect-disk-sequence` to checker,
inspector and mount commands; they compare expectations against the claimed
descriptor before recovery, closing the discovery-to-open hotplug race.

Software remains 0.0.1, experimental format revision 2. The formatter uses
65,536 inodes. No backups are made or changed. Existing repository `mnt/` files
are never used. Data elsewhere on the disk is not securely erased.

## Prerequisites and disposable tests

Linux sysfs diskseq, FUSE (`/dev/fuse`, `fusermount3`), util-linux (`lsblk`,
`blockdev`, `losetup`, `partx`, `sfdisk`, `mount`, `umount`), ext4 tools, Python 3,
GNU nano, Rust 1.89 or newer, and sudo access are needed. Run from the repository
root as your normal user:

```sh
python3 scripts/device-validation.py
```

This builds the workspace and asks sudo to run **only disposable loop tests**,
then the real loop-backed FUSE tests. It never selects a USB drive. The backend
and formatter cover both sector sizes, unaligned I/O, exclusive claims, writable
reopen, capacity changes, and mounted-child refusal. FUSE tests check dropped
UID/GID/groups, ownership, access exclusion, writable operations, clean remount,
and full-image read-only byte preservation. Loop autoclear is not physical
removal; physical removal is tested below. Unit tests also exercise a vanished
sysfs identity and a changed disk sequence without reopening the retained file.

The hardware workload can also be exercised safely on a temporary image:

```sh
python3 scripts/acceptance-image-smoke.py
python3 -m unittest discover -s tests/hardware -v
```

The first command uses real FUSE and nano but **does not count as USB acceptance**.
The second checks rejection of lost acknowledged data, invalid rename outcomes,
incorrect truncation recovery and unexpected namespace entries using host files.
Ordinary `cargo test` ignores all privileged loop tests and never erases hardware.

## Formatting and full acceptance

The following formatting command destroys the selected drive's old format and
contents. It is separate from every reusable workload/recovery command. Use a
new evidence directory on the host; the formatter refuses a second format
attempt in the same evidence directory.

```sh
sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$PWD/usb-evidence-2026-09-28" \
  format --erase

sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$PWD/usb-evidence-2026-09-28" \
  accept
```

`accept` never formats. It runs the workload, clean reconnect, three abrupt
removal trials, and final verification in sequence, stopping on any failure.
It prompts for physical unplug/reconnect in your terminal. Keep the drive
unplugged until asked to reconnect: the old service must release its descriptor.
Each command checks that removal was actually observed and that reconnect has a
new disk sequence with the same serial and capacity. No response or unobserved
removal can count as a pass. The process has per-command and worker deadlines.

The same phases are individually reusable by replacing `accept` with:

- `workload`: directories, create/write/append, copy, rename/replacement, delete,
  truncate, sparse data, executable flags, timestamps, open-unlinked handles,
  a scripted real nano save, free-block/inode restoration, durable throughput,
  host manifests, device exclusion and read-only no-write verification.
- `reconnect`: clean unplug/reconnect, check and compare all content/namespace;
  records a read measurement after actual physical reconnection.
- `trial create`: create and append to new files; completed files become immutable.
- `trial replace`: prepare old/new files and atomically replace each old name.
- `trial cleanup`: truncate and delete newly created files.
- `verify`: inspect read-only, validate the recovered view, mount writable to
  complete recovery, compare acknowledged content and validate interrupted
  outcomes. It never reformats or silently restarts a trial.
- `finish`: the same verification followed by a final check and clean unmount;
  `final.json` lists any missing acceptance phases instead of declaring success.

After a failed/interrupted harness command, retain all evidence and use `verify`.
A verification-only recovery is recorded separately; it does not establish that
an observed physical unplug trial passed. Investigation is required before
advancing that trial. Never reformat to conceal a failure.

## Evidence and limits

All manifests, hashes, per-operation intentions/acknowledgements, worker output,
service output and checker reports live in the host evidence directory. Intent
records are fsynced before filesystem operations. Phase acknowledgements are
fsynced only after successful file/directory synchronization. Baseline immutable
data must match exactly. Interrupted unacknowledged create/append can leave an
absent or partial prefix; replacement can leave the old+temporary pair or the
new name; truncation/deletion outcomes are constrained by the last acknowledged
phase. The recovered-view checker validates namespace and allocation ownership.

Read-only hardware verification compares the disk's sectors-written counter
before and after service, in addition to attempting a mutation and requiring
EROFS. Disposable loops additionally compare every backing-image byte.
`performance.json` measures an 8 MiB write including fsync, through FUSE and the
buffered backend. The later read is measured after physical reconnect. Neither
measurement is raw USB link speed; no image/tmpfs measurement is labeled USB.

Each mount uses a newly created temporary mountpoint. Clean paths use normal
unmount; disconnected services use lazy detach only during abrupt-removal
cleanup. Workers and services have bounded waits and termination. If unmount
fails, the mountpoint is retained and reported; the hardware harness never
recursively deletes a possibly mounted directory.

The final drive is left XFFS and cleanly unmounted after a successful sequence.
Incomplete in-place overwrites retain the existing mixed-old/new data limitation.
Partition formatting/resizing, restoration of Ventoy, desktop auto-mounting,
kernel drivers, migration and stable compatibility remain out of scope.
