#!/usr/bin/env python3
"""Exercise USB workload code on a temporary image; NOT a hardware acceptance pass."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('usb_acceptance', ROOT / 'scripts/usb-acceptance.py')
usb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usb)

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
                usb.run(['python3', ROOT / 'scripts/usb-acceptance.py', '_worker', mode, mount, report], timeout=180)
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
