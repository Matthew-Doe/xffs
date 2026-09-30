#!/usr/bin/env python3
"""Exercise USB workload code on a temporary image; NOT a hardware acceptance pass."""
import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('usb_acceptance', ROOT / 'scripts/usb-acceptance.py')
usb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usb)

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path)
parser.add_argument('--resume', action='store_true')
options = parser.parse_args()
if options.bin_dir:
    usb.BIN = options.bin_dir.resolve()

with tempfile.TemporaryDirectory(prefix='xffs-acceptance-image-') as tmp:
    tmp = Path(tmp)
    image, mount, report = tmp / 'disk.img', tmp / 'mount', tmp / 'evidence'
    mount.mkdir()
    report.mkdir()
    usb.run([usb.BIN / 'mkfs-xffs', image, '--size-mib', '64', '--uuid', '58464653-0000-0001-8000-000000000001'])
    for writable in [True, False]:
        with (tmp / 'service.log').open('w+') as log:
            args = [usb.BIN / 'mount-xffs', image, mount]
            if writable:
                args.append('--rw')
            proc = subprocess.Popen(args, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while not usb.is_mounted(mount):
                    if proc.poll() is not None or time.monotonic() > deadline:
                        log.seek(0)
                        raise RuntimeError(log.read())
                    time.sleep(.05)
                mode = 'workload' if writable else 'verify'
                if options.resume:
                    if writable:
                        (mount / 'persist').mkdir()
                        usb.write_sync(mount / 'persist/throughput.bin', b'partial evidence')
                        (mount / 'acceptance-work').mkdir()
                        usb.write_sync(mount / 'acceptance-work/retained', b'old work')
                        usb.write_sync(mount / 'test.txt', b'user file')
                        mode = 'resume-workload'
                    else:
                        mode = 'verify-cold'
                usb.run(['python3', ROOT / 'scripts/usb-acceptance.py', '_worker', mode, mount, report], timeout=180)
                if writable:
                    usb.trial_worker(mount, report, 'cow', iterations=2)
                    usb.verify(mount, report, trial='cow')
                    usb.save(report / 'baseline.json', usb.snapshot(mount))
                else:
                    usb.verify_trial(mount, report, 'cow')
                if options.resume:
                    assert (mount / 'persist/throughput.bin').read_bytes() == b'partial evidence'
                    assert (mount / 'acceptance-work/retained').read_bytes() == b'old work'
                    assert (mount / 'test.txt').read_bytes() == b'user file'
                if not writable:
                    usb.run(['python3', ROOT / 'scripts/usb-acceptance.py', '_worker', 'readonly', mount, report])
            finally:
                if usb.is_mounted(mount):
                    usb.run(['fusermount3', '-u', mount])
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
        assert proc.returncode == 0
        usb.run([usb.BIN / 'xffs-check', image])
print('PASS: acceptance workload, real nano, manifest remount, and RO behavior on temporary image (NOT USB)')
