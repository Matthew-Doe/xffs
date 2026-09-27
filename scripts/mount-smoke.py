#!/usr/bin/env python3
"""Explicit real Linux FUSE integration test. Exit 77 means unmet prerequisite."""
import errno
import filecmp
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent

def run(args, **kwargs):
    return subprocess.run(args, check=True, timeout=30, **kwargs)

def mounted(path):
    return subprocess.run(['mountpoint', '-q', str(path)], timeout=5).returncode == 0

def timed_out(_signal, _frame):
    raise TimeoutError("mount smoke test exceeded overall timeout")

def main():
    signal.signal(signal.SIGTERM, timed_out)
    missing = [x for x in ['fusermount3', 'mountpoint', 'cargo'] if not shutil.which(x)]
    if not Path('/dev/fuse').exists():
        missing.append('/dev/fuse')
    if missing:
        print('UNMET PREREQUISITE: ' + ', '.join(missing), file=sys.stderr)
        return 77
    subprocess.run(['cargo', 'build', '--workspace', '--locked'], cwd=ROOT, check=True, timeout=120)
    target = ROOT / 'target' / 'debug'
    with tempfile.TemporaryDirectory(prefix='xffs-mount-smoke-') as tmp:
        tmp = Path(tmp)
        mountpoint = tmp / 'mnt'
        mountpoint.mkdir()
        for scenario in ['clean', 'committed']:
            image = tmp / (scenario + '.img')
            backup = tmp / (scenario + '.before')
            run([target / 'xffs-image', 'create-demo', image, '--scenario', scenario])
            run([target / 'xffs-check', image])
            shutil.copyfile(image, backup)
            # Repeated mounts, then noexec, must leave the entire image identical.
            for noexec in [False, True]:
                with (tmp / 'mount.log').open('w+') as log:
                    cmd = [target / 'mount-xffs', image, mountpoint]
                    if noexec:
                        cmd.append('--noexec')
                    proc = subprocess.Popen(cmd, stdout=log, stderr=log)
                    try:
                        deadline = time.monotonic() + 10
                        while not mounted(mountpoint):
                            if proc.poll() is not None or time.monotonic() >= deadline:
                                log.seek(0)
                                raise RuntimeError('mount failed or timed out: ' + log.read())
                            time.sleep(0.05)
                        run(['ls', '-la', mountpoint])
                        run(['find', mountpoint, '-type', 'f'], stdout=subprocess.DEVNULL)
                        run(['stat', mountpoint / 'sparse.bin'])
                        result = run(['cat', mountpoint / 'ReadMe.txt'], capture_output=True)
                        assert result.stdout == b'Hello from XFFS!\n'
                        assert (mountpoint / 'CAFE\u0301.TXT').read_bytes() == b'Canonical caseless lookup.\n'
                        assert (mountpoint / 'Recovered.txt').read_bytes() == b'Journal recovery is visible.\n'
                        assert not (mountpoint / 'Before.txt').exists()
                        assert (mountpoint / 'nested/binary.bin').read_bytes() == bytes(range(256))*32
                        assert (mountpoint / 'overflow.bin').read_bytes() == b'\x5a'*(5*4096)
                        assert len(list((mountpoint / 'many').iterdir())) == 160
                        with (mountpoint / 'sparse.bin').open('rb') as stream:
                            stream.seek((1 << 32)-4)
                            assert stream.read(8) == b'\0'*4+b'\x77'*4
                            stream.seek(4096)
                            assert stream.read(4096) == b'\0'*4096
                            os.fsync(stream.fileno())
                        if not noexec:
                            assert run([mountpoint / 'run.sh'], capture_output=True).stdout == b'XFFS demo\n'
                        else:
                            try:
                                run([mountpoint / 'run.sh'])
                            except PermissionError:
                                pass
                            else:
                                raise AssertionError('noexec allowed execution')
                        for mutation in [lambda: (mountpoint/'new').write_bytes(b'x'),
                                         lambda: (mountpoint/'ReadMe.txt').write_bytes(b'x'),
                                         lambda: (mountpoint/'ReadMe.txt').rename(mountpoint/'renamed'),
                                         lambda: (mountpoint/'ReadMe.txt').unlink(),
                                         lambda: (mountpoint/'newdir').mkdir(),
                                         lambda: (mountpoint/'ReadMe.txt').chmod(0o666)]:
                            try:
                                mutation()
                            except OSError as error:
                                assert error.errno == errno.EROFS, error
                            else:
                                raise AssertionError('mutation unexpectedly succeeded')
                    finally:
                        try:
                            if mounted(mountpoint):
                                try:
                                    run(['fusermount3', '-u', mountpoint])
                                finally:
                                    if mounted(mountpoint):
                                        run(['fusermount3', '-uz', mountpoint])
                        finally:
                            try:
                                proc.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                proc.terminate()
                                try:
                                    proc.wait(timeout=5)
                                except subprocess.TimeoutExpired:
                                    proc.kill()
                                    proc.wait(timeout=5)
                    assert proc.returncode == 0, proc.returncode
                    assert filecmp.cmp(image, backup, shallow=False), 'mount changed image'
    print('PASS: real clean/recovered read-only mounts; complete images unchanged')
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
