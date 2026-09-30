#!/usr/bin/env python3
"""Explicit root-only disposable loop/FUSE tests. Never selects physical disks."""
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / 'target/debug'

def run(args, **kwargs):
    return subprocess.run([str(x) for x in args], check=True, timeout=45, **kwargs)

def mounted(path):
    return subprocess.run(['mountpoint', '-q', str(path)], timeout=5).returncode == 0

def workload(path, uid, gid, writable):
    assert path.stat().st_uid == uid
    assert path.stat().st_gid == gid
    if not writable:
        try:
            (path / 'forbidden').write_text('no')
        except OSError as error:
            import errno
            assert error.errno == errno.EROFS
        else:
            raise AssertionError('read-only mount allowed mutation')
        return
    baseline = os.statvfs(path).f_bfree
    folder = path / 'test'
    folder.mkdir()
    p = folder / 'file'
    with p.open('wb') as f:
        f.write(b'hello')
        f.flush()
        os.fsync(f.fileno())
    with p.open('ab') as f:
        f.write(b' world')
        f.flush()
        os.fsync(f.fileno())
    assert p.read_bytes() == b'hello world'
    p.rename(folder / 'renamed')
    p = folder / 'renamed'
    with p.open('r+b') as f:
        f.truncate(1 << 20)
        f.seek(4096)
        assert f.read(32) == bytes(32)
        p.unlink()
        f.seek(0)
        assert f.read(11) == b'hello world'
    folder.rmdir()
    assert os.statvfs(path).f_bfree == baseline
    (path / 'retained').write_bytes(b'reconnect persistence')

def main():
    if len(sys.argv) > 1 and sys.argv[1] == '--workload':
        workload(Path(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5] == 'rw')
        return 0
    if os.geteuid() != 0 or not Path('/dev/fuse').exists():
        print('UNMET PREREQUISITE: run through sudo with /dev/fuse available')
        return 77
    uid, gid = int(os.environ.get('SUDO_UID', '0')), int(os.environ.get('SUDO_GID', '0'))
    if not uid or not gid:
        print('UNMET PREREQUISITE: non-root SUDO_UID and SUDO_GID')
        return 77
    for sector in [512, 4096]:
        with tempfile.TemporaryDirectory(prefix='xffs-device-fuse-') as tmp:
            tmp = Path(tmp)
            os.chown(tmp, uid, gid)
            mount = tmp / 'mount'
            mount.mkdir()
            os.chown(mount, uid, gid)
            image = tmp / 'disk.img'
            run([BIN / 'mkfs-xffs', image, '--size-mib', '32', '--uuid', '58464653-0000-0001-8000-000000000001'])
            device = run(['losetup', '--find', '--show', '--sector-size', sector, image], capture_output=True, text=True).stdout.strip()
            try:
                run(['udevadm', 'settle', '--timeout=15'])
                for mode in ['ro', 'rw', 'ro']:
                    before = hashlib.sha256(image.read_bytes()).hexdigest()
                    with (tmp / 'service.log').open('w+') as log:
                        command = [BIN / 'mount-xffs', device, mount, '--device', '--uid', str(uid), '--gid', str(gid)]
                        if mode == 'rw':
                            command.append('--rw')
                        proc = subprocess.Popen(command, stdout=log, stderr=log)
                        try:
                            deadline = time.monotonic() + 15
                            while not mounted(mount):
                                if proc.poll() is not None or time.monotonic() > deadline:
                                    log.seek(0)
                                    raise RuntimeError(log.read())
                                time.sleep(.05)
                            status = Path(f'/proc/{proc.pid}/status').read_text()
                            fields = dict(line.split(':', 1) for line in status.splitlines() if ':' in line)
                            assert fields['Uid'].split() == [str(uid)] * 4
                            assert fields['Gid'].split() == [str(gid)] * 4
                            assert not fields['Groups'].split()
                            conflict = subprocess.run([BIN / 'xffs-check', device, '--device'], capture_output=True, timeout=10)
                            assert conflict.returncode != 0 and b'LockContention' in conflict.stderr
                            run([sys.executable, __file__, '--workload', mount, uid, gid, mode], user=uid, group=gid, extra_groups=[])
                        finally:
                            if mounted(mount):
                                run(['fusermount3', '-u', mount], user=uid, group=gid, extra_groups=[])
                            try:
                                proc.wait(timeout=10)
                            except subprocess.TimeoutExpired:
                                proc.kill()
                                proc.wait(timeout=5)
                    assert proc.returncode == 0
                    run([BIN / 'xffs-check', device, '--device'])
                    if mode == 'ro':
                        assert hashlib.sha256(image.read_bytes()).hexdigest() == before
                print(f'PASS: {sector}-byte loop device, ownership, privilege drop, RO hash, RW workload, exclusion')
            finally:
                run(['losetup', '-d', device])
    return 0

if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(TimeoutError('terminated')))
    sys.exit(main())
