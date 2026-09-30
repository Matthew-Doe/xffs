#!/usr/bin/env python3
"""Real image-file acceptance, including a scripted interactive nano save."""
import argparse
import contextlib
import errno
import hashlib
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / 'target/debug'


def run(args, **kwargs):
    return subprocess.run(args, check=True, timeout=30, **kwargs)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


@contextlib.contextmanager
def mount(image, point, *options):
    with tempfile.TemporaryFile(mode='w+') as log:
        proc = subprocess.Popen([TARGET/'mount-xffs', image, point, *options], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while not os.path.ismount(point):
                if proc.poll() is not None or time.monotonic() > deadline:
                    log.seek(0)
                    raise RuntimeError('mount failed: ' + log.read())
                time.sleep(0.05)
            yield
        finally:
            try:
                if os.path.ismount(point):
                    try:
                        run(['fusermount3', '-u', point])
                    finally:
                        if os.path.ismount(point):
                            run(['fusermount3', '-uz', point])
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
            if proc.returncode:
                log.seek(0)
                raise RuntimeError('mount process failed: ' + log.read())


def nano_save(path):
    pid, terminal = pty.fork()
    if pid == 0:
        os.environ.update(TERM='xterm', LC_ALL='C')
        os.execvp('nano', ['nano', '-I', '-w', str(path)])
    exited = False
    transcript = bytearray()

    def expect(marker):
        deadline = time.monotonic() + 10
        while not any(m in transcript for m in (marker if isinstance(marker, tuple) else (marker,))):
            if time.monotonic() > deadline:
                raise TimeoutError('nano prompt missing: ' + repr(marker) + '; terminal: ' + repr(bytes(transcript[-4096:])))
            readable, _, _ = select.select([terminal], [], [], 0.1)
            if readable:
                transcript.extend(os.read(terminal, 65536))
        transcript.clear()

    try:
        expect(b'Go To Line')
        os.write(terminal, b'Saved by a real nano session.\x0f')
        expect((b'File Name to Write', b'Write to File:'))
        os.write(terminal, b'\r')
        expect((b'Written', b'Wrote'))
        os.write(terminal, b'\x18')
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            child, status = os.waitpid(pid, os.WNOHANG)
            if child:
                exited = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            time.sleep(0.05)
        if not exited:
            raise TimeoutError('nano did not exit')
    finally:
        if not exited:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(terminal)
    assert path.read_bytes() == b'Saved by a real nano session.\n', repr(path.read_bytes())


def main():
    global TARGET
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', type=Path, help='use prebuilt executables without building')
    args = parser.parse_args()
    if args.bin_dir:
        TARGET = args.bin_dir.resolve()
    def timed_out(_sig, _frame):
        raise TimeoutError('writable smoke test exceeded overall timeout')
    signal.signal(signal.SIGTERM, timed_out)
    required = ['cargo', 'fusermount3', 'nano', 'mkdir', 'touch', 'cat', 'cp', 'mv', 'rm', 'rmdir', 'truncate', 'sh']
    missing = [name for name in required if not shutil.which(name) and (name != 'cargo' or not args.bin_dir)]
    if not Path('/dev/fuse').exists():
        missing.append('/dev/fuse')
    if missing:
        print('UNMET PREREQUISITE: ' + ', '.join(missing), file=sys.stderr)
        return 77

    if not args.bin_dir:
        subprocess.run(['cargo', 'build', '--workspace', '--locked'], cwd=ROOT, check=True, timeout=120)
    with tempfile.TemporaryDirectory(prefix='xffs-writable-smoke-') as temp:
        temp = Path(temp)
        point = temp/'mnt'
        point.mkdir()
        image = temp/'disk.img'
        uuid = '58464653-0000-0001-8000-000000000001'
        run([TARGET/'mkfs-xffs', image, '--size-mib', '32', '--uuid', uuid])
        with mount(image, point, '--rw'):
            initial = os.statvfs(point)
            # An exclusive writer excludes both readers and another writer.
            for cmd in [[TARGET/'xffs-check', image], [TARGET/'mount-xffs', image, temp, '--rw']]:
                result = subprocess.run(cmd, capture_output=True, timeout=5)
                assert result.returncode != 0 and b'lock' in result.stderr.lower(), result.stderr
            run(['mkdir', point/'work'])
            run(['touch', point/'work/empty'])
            file = point/'work/text'
            run(['sh', '-c', 'printf "hello" > "$1"; printf " world\\n" >> "$1"', 'sh', file])
            assert run(['cat', file], capture_output=True).stdout == b'hello world\n'
            run(['cp', file, point/'copy'])
            run(['mv', point/'copy', point/'moved'])
            run(['truncate', '-s', '5', point/'moved'])
            assert (point/'moved').read_bytes() == b'hello'
            run(['truncate', '-s', '12', point/'moved'])
            assert (point/'moved').read_bytes() == b'hello'+b'\0'*7
            nano_save(point/'nano.txt')
            # Multiple opens see writes immediately; append ignores old offsets.
            with file.open('r+b', buffering=0) as first, file.open('rb', buffering=0) as second:
                first.write(b'HELLO')
                assert second.read(5) == b'HELLO'
                with file.open('ab', buffering=0) as a, file.open('ab', buffering=0) as b:
                    a.write(b'A'); b.write(b'B')
                second.seek(0)
                assert second.read() == b'HELLO world\nAB'
                os.fsync(first.fileno())
            before = os.statvfs(point).f_bfree
            orphan = point/'orphan'
            orphan.write_bytes(b'open')
            with orphan.open('r+b', buffering=0) as handle:
                orphan.unlink()
                assert os.fstat(handle.fileno()).st_nlink == 0
                assert handle.read() == b'open'
                handle.write(b' unlinked')
                handle.seek(0)
                assert handle.read() == b'open unlinked'
                os.fsync(handle.fileno())
            assert os.statvfs(point).f_bfree == before
            # Atomic replacement retains the prior contents for existing opens.
            (point/'target').write_bytes(b'old')
            with (point/'target').open('rb', buffering=0) as old:
                with (point/'temporary').open('wb', buffering=0) as new:
                    new.write(b'new'); os.fsync(new.fileno())
                os.replace(point/'temporary', point/'target')
                assert old.read() == b'old'
                assert (point/'target').read_bytes() == b'new'
                fd = os.open(point, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            executable = point/'run.sh'
            executable.write_bytes(b'#!/bin/sh\nprintf "executable\\n"\n')
            executable.chmod(0o744)
            assert executable.stat().st_mode & 0o777 == 0o755
            assert run([executable], capture_output=True).stdout == b'executable\n'
            os.utime(executable, (123456, 234567))
            assert (executable.stat().st_atime, executable.stat().st_mtime) == (123456, 234567)
            for mode in [0o600, 0o666, 0o4755]:
                try:
                    executable.chmod(mode)
                except OSError as e:
                    assert e.errno == errno.EOPNOTSUPP
                else:
                    raise AssertionError('unrelated chmod succeeded')
            run(['rm', point/'work/empty'])
            run(['mv', file, point/'text'])
            run(['rmdir', point/'work'])
            start = time.monotonic()
            (point/'perf').write_bytes(b'x'*(1024*1024))
            elapsed = time.monotonic()-start
            print(f'PERFORMANCE: 1 MiB durable write {elapsed:.3f}s ({1/elapsed:.2f} MiB/s)', flush=True)
            (point/'perf').unlink()
            expected = {p.name: p.read_bytes() for p in point.iterdir()}
            free = os.statvfs(point)
        run([TARGET/'xffs-check', image])
        run([TARGET/'xffs-inspect', image], stdout=subprocess.DEVNULL)
        with mount(image, point, '--rw', '--noexec'):
            assert {p.name: p.read_bytes() for p in point.iterdir()} == expected
            stats = os.statvfs(point)
            assert (stats.f_bfree, stats.f_ffree) == (free.f_bfree, free.f_ffree)
            try:
                run([point/'run.sh'])
            except PermissionError:
                pass
            else:
                raise AssertionError('noexec allowed execution')
            for p in point.iterdir():
                p.unlink()
            stats = os.statvfs(point)
            assert (stats.f_bfree, stats.f_ffree) == (initial.f_bfree, initial.f_ffree)
        run([TARGET/'xffs-check', image])
        before = digest(image)
        with mount(image, point):
            assert list(point.iterdir()) == []
        assert digest(image) == before
        old = temp/'revision-one.img'
        run([TARGET/'mkfs-xffs', old, '--size-mib', '16', '--uuid', uuid, '--format-revision', '1'])
        before = digest(old)
        result = subprocess.run([TARGET/'mount-xffs', old, point, '--rw'], capture_output=True, timeout=10)
        assert result.returncode != 0 and b'Unsupported' in result.stderr, result.stderr
        with mount(old, point):
            assert list(point.iterdir()) == []
        assert digest(old) == before
    print('PASS: writable terminal operations, nano, durability, lifetimes, permissions, locking and remount')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
