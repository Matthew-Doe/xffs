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

`accept` never formats. It runs the workload, clean reconnect, four abrupt
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
- `trial cow`: initialize and sync files, then overwrite existing blocks without
  truncate or rename. Alternates three full blocks and unaligned partial updates
  across three blocks. Checks unchanged bytes, old-or-new block contents,
  committed prefixes, and survival of acknowledged overwrites.
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
The updated writer uses per-block file-data COW under the storage contract;
multi-block requests can recover as a committed prefix. This does not certify
that a physical device honors flushes. Older writers retain the mixed-data limitation.
Partition formatting/resizing, restoration of Ventoy, desktop auto-mounting,
kernel drivers, migration and stable compatibility remain out of scope.

## Cancelling a slow workload

The workload prints stages, an activity message every five seconds, and durable
progress for each MiB of the throughput write. USB transaction flushes can make
this phase take substantially longer than the tmpfs image test. Ctrl+C cancels
the worker first; the FUSE service is isolated from terminal signals so cleanup
can unmount it normally. Interrupted files and host evidence are preserved.

If a baseline workload was interrupted before `baseline.json` was written, use
`recover` instead of `verify` or repeating `workload`:

```sh
sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$PWD/usb-evidence-2026-09-28-manual" \
  recover
```

This checks the recovered view, opens writable to finish recovery, saves a dated
`*-recovery-inventory.json`, unmounts, and checks again. It does not delete or
rewrite test files, replace the baseline, or establish an acceptance pass.
A subsequent workload refuses existing `acceptance-work` or `persist` entries;
inspect the retained files before deciding how to resume the test.

After clean unmount, another device probe may briefly hold a conflicting claim.
Read-only `check` and raw inspection retry only an exact `LockContention` failure
for up to 15 seconds of contention waiting, recording every attempt. Serial and
disk-sequence expectations remain unchanged. Identity and filesystem errors are
not retried; formatting and writable recovery are never automatically retried.
The standalone `check` action performs only read-only validation, so it can finish
a post-unmount check without rerunning a workload or writable recovery.

After recovery and inspection, `resume-workload` runs the baseline workload in
new UUID-suffixed directories. It preserves existing test/user files and records
their hashes before writing, then verifies those entries are unchanged before
publishing the new baseline. It does not reformat, delete old partial files, or
replace a completed baseline. The throughput file's relative path is recorded
in `performance.json` for subsequent reconnect measurements.

```sh
sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$PWD/usb-evidence-2026-09-28-manual" \
  resume-workload
```

Leave the USB connected until the command finishes. Once it passes, use
`reconnect` to compare the completed baseline across a coordinated clean unplug,
then proceed to the abrupt-removal trials. An earlier reconnect without a
completed baseline is useful validation but does not replace that comparison.

## Early Enter during reconnection

Both reconnect prompts wait up to 60 seconds after Enter for the selected serial
to enumerate with a new disk sequence and the same capacity. Temporary absence
or a disappearing sysfs entry is retried; duplicate identity, wrong transport,
and wrong capacity remain errors. The original disk sequence is never accepted.

If a trial stopped after its physical removal was recorded but before reconnect
verification, resume that verification without starting another workload:

```sh
sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$PWD/usb-evidence-2026-09-28-manual" \
  resume-trial replace
```

`resume-trial KIND` requires the original intentions log and an observed-removal
event for that kind, refuses already-passed trials, and runs the same read-only
validation, writable recovery, acknowledgement checks and final check as the
original trial. Only then does it mark the trial passed. It does not repeat the
unplug or reformat. Without an observed-removal record, use `verify`; that alone
still cannot establish a physical-removal pass.

## Completing a missing format receipt without erasing

If on-disk formatting succeeded but the original post-format reopen or partition
refresh failed, `finalize-report` verifies the existing disk against the original
format attempt. It requires matching serial, current disk sequence, capacity,
UUID, revision 2, 65,536 inodes, two matching valid superblocks and a valid
recovered filesystem. It then refreshes the kernel partition view, waits for
udev, checks that no partitions remain, and repeats the same read-only format
verification. Only after success does it create a missing `formatted.json`,
explicitly identifying it as later verification rather than claiming that the
original formatter command succeeded. It then runs the normal `finish` checks.

```sh
cargo build -p xffs-tools --bin xffs-verify-format --locked
sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$PWD/usb-evidence-2026-09-28-manual" \
  finalize-report
```

No mkfs operation is invoked and no file data is erased. Existing evidence is
preserved. A failed verification or refresh leaves the missing receipt missing;
do not manually invent a passing receipt or reformat to fix a reporting gap.


## Adding COW verification to an existing acceptance run

Keep the existing drive and evidence; do not format or repeat `accept`.
For the completed September 30 run, use these fish-compatible commands:

```fish
set REPORT /home/matthewd/devel/xffs/usb-evidence-20260930-123545
sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$REPORT" trial cow
and sudo python3 scripts/usb-acceptance.py \
  --expect-serial 0085199340190280 --report-dir "$REPORT" finish
```

Follow the unplug/reconnect prompts. The COW trial publishes readiness only after
the first old file is durable and its overwrite intent is recorded on the host.
An unacknowledged overwrite may recover any complete block prefix; torn blocks,
non-prefix replacements, missing initialized files, size changes, and lost
acknowledged overwrites fail verification. Initialization interrupted on later
files is checked separately. New and old bytes differ throughout every updated
range. The existing filesystem checker validates recovered allocation ownership.

New full acceptance runs include COW and require `passed-cow.json`; earlier
reports remain historical evidence for their original three trials. Interrupted
COW trials use the existing `verify` / `resume-trial cow` workflow.
A physical trial samples an uncontrolled failure point; it does not replace
the simulator's exhaustive write/flush-boundary tests.
