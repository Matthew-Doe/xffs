#!/usr/bin/env python3
"""Profile XFFS writes without changing durability. Device mode NEVER formats."""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('usb_profile', ROOT / 'scripts/usb-acceptance.py')
usb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usb)


def worker(mount, report, name, phase, mib):
    path = mount / name
    expected = bytes([53 if phase == 'create' else 202]) * (1024 * 1024)
    before = os.statvfs(mount)
    if phase == 'cleanup':
        path.unlink()
        usb.sync_dir(mount)
        after = os.statvfs(mount)
        initial = json.loads((report / 'create-workload.json').read_text())
        assert (after.f_bfree, after.f_ffree) == (initial['free_blocks'], initial['free_inodes'])
        usb.save(report / 'cleanup.json', {'passed': True, 'free_space_restored': True})
        return
    if phase == 'overwrite':
        assert path.stat().st_size == mib * len(expected)
    mode = 'xb' if phase == 'create' else 'r+b'
    with path.open(mode, buffering=0) as f:
        start = time.monotonic()
        for _ in range(mib):
            remaining = memoryview(expected)
            while remaining:
                n = f.write(remaining)
                if not n:
                    raise OSError('write made no progress')
                remaining = remaining[n:]
        os.fsync(f.fileno())
        elapsed = time.monotonic() - start
    usb.sync_dir(mount)
    with path.open('rb', buffering=0) as f:
        for _ in range(mib):
            assert f.read(len(expected)) == expected, 'profile workload content mismatch'
        assert f.read(1) == b''
    usb.save(report / (phase + '-workload.json'), {
        'bytes': mib * len(expected), 'write_fsync_seconds': elapsed,
        'mib_s': mib / elapsed, 'verified': True,
        'free_blocks': before.f_bfree, 'free_inodes': before.f_ffree})


@contextmanager
def image_mount(image, report, binary, phase, enabled=True):
    mount = report / 'mount'
    mount.mkdir(exist_ok=True)
    args = [binary, image, mount, '--rw']
    if phase != 'cleanup' and enabled:
        args += ['--profile-json', report / (phase + '-profile.json')]
    with (report / (phase + '-mount.log')).open('w') as log:
        proc = subprocess.Popen([str(x) for x in args], stdout=log, stderr=log)
    try:
        deadline = time.monotonic() + 15
        while not usb.is_mounted(mount):
            if proc.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('mount failed; see mount log')
            time.sleep(.05)
        yield mount
    finally:
        if usb.is_mounted(mount):
            usb.run(['fusermount3', '-u', mount])
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.terminate()
            proc.wait(timeout=10)
        if proc.returncode:
            raise RuntimeError('mount exited unsuccessfully; evidence retained')


def summarize(report, enabled=True):
    summary = {}
    for phase in ('create', 'overwrite'):
        work = json.loads((report / (phase + '-workload.json')).read_text())
        metrics = json.loads((report / (phase + '-profile.json')).read_text())['timings'] if enabled else {}
        summary[phase] = {'workload': work, 'timings': metrics}
        print(f"\n{phase}: {work['write_fsync_seconds']:.3f}s, {work['mib_s']:.3f} MiB/s")
        print('Inclusive timings: nested rows overlap; do not add all rows.')
        for name, metric in sorted(metrics.items()):
            print(f"{name:26} {metric['count']:7} calls {metric['total_ns']/1e9:9.3f}s "
                  f"p95 <= {metric['p95_upper_ns']/1e6:8.3f}ms "
                  f"errors={metric['errors']}")
    usb.save(report / 'summary.json', summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--no-profile', action='store_true', help='run identical workload without timing counters')
    parser.add_argument('--device', action='store_true', help='use the serial-bound existing USB filesystem')
    parser.add_argument('--expect-serial', choices=[usb.SERIAL])
    parser.add_argument('--output', required=True, type=Path, help='new directory on host storage')
    parser.add_argument('--size-mib', type=int, default=8)
    parser.add_argument('--bin-dir', type=Path, default=ROOT / 'target/release')
    args = parser.parse_args()
    if not 1 <= args.size_mib <= 64:
        parser.error('--size-mib must be 1..64')
    if args.device != bool(args.expect_serial):
        parser.error('--device and --expect-serial must be supplied together')
    report = args.output.resolve()
    if report.exists():
        parser.error('--output must be a new directory')
    usb.BIN = args.bin_dir.resolve()
    name = '.xffs-profile-' + uuid.uuid4().hex
    if args.device:
        device = usb.identify()
        harness = usb.Harness(report, device)
        harness.check(device)
    else:
        if os.geteuid() == 0:
            parser.error('run image profiling as your ordinary user')
        report.mkdir(parents=True)
        harness = None
    provenance = {
        'scope': 'USB' if args.device else 'host-backed temporary image',
        'temporary_file': name, 'mib_per_phase': args.size_mib,
        'device': device if args.device else None,
        'mount_binary': str(usb.BIN / 'mount-xffs'),
        'mount_binary_sha256': hashlib.sha256((usb.BIN / 'mount-xffs').read_bytes()).hexdigest(),
        'startup_excluded': True, 'nested_metrics_inclusive': True,
        'profiling_enabled': not args.no_profile,
    }
    usb.save(report / 'run.json', provenance)
    # In device mode this temporary directory is never formatted or used.
    with tempfile.TemporaryDirectory(prefix='xffs-profile-image-') as tmp:
        image = Path(tmp) / 'disk.img'
        if not args.device:
            usb.run([usb.BIN / 'mkfs-xffs', image, '--size-mib', str(max(64, args.size_mib * 2)),
                     '--uuid', str(uuid.uuid4())])
        for phase in ('create', 'overwrite', 'cleanup'):
            context = (harness.mount(device, writable=True,
                       profile_json=report / (phase + '-profile.json') if phase != 'cleanup' and not args.no_profile else None)
                       if harness else image_mount(image, report, usb.BIN / 'mount-xffs', phase, not args.no_profile))
            with context as mount:
                print(f'{phase}: profiling {args.size_mib} MiB; keep the device connected', flush=True)
                with (report / (phase + '-worker.log')).open('w') as log:
                    subprocess.run([sys.executable, __file__, '_worker', str(mount), str(report),
                                    name, phase, str(args.size_mib)], check=True, timeout=600,
                                   stdout=log, stderr=subprocess.STDOUT,
                                   **(harness.user if harness else {}))
            if harness:
                harness.check(device)
            else:
                usb.run([usb.BIN / 'xffs-check', image])
    summarize(report, not args.no_profile)
    print(f'PASS: verified contents, cleanup and free-space restoration. Evidence: {report}')


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '_worker':
        worker(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4], sys.argv[5], int(sys.argv[6]))
    else:
        main()
