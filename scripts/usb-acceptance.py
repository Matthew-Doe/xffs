#!/usr/bin/env python3
"""Explicit acceptance harness for serial 0085199340190280. Never run by cargo test.
Formatting is a separate --erase action. All evidence lives on the host.
"""
import argparse
from contextlib import contextmanager
import errno
import hashlib
import json
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
import uuid

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / 'target/debug'
SERIAL = '0085199340190280'
TRIALS = ['create', 'replace', 'cleanup', 'cow']


def run(args, **kwargs):
    kwargs.setdefault('timeout', 60)
    return subprocess.run([str(x) for x in args], check=True, **kwargs)


def progress(message):
    print(message, flush=True)


def stop_worker(proc):
    if proc.poll() is None:
        # Give Python finally blocks (including nano cleanup) a chance to run.
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=10)


def wait_worker(proc, logpath, timeout=180):
    start = time.monotonic()
    heartbeat = start + 5
    with logpath.open() as output:
        while True:
            text = output.read()
            if text:
                print(text, end='', flush=True)
            status = proc.poll()
            if status is not None:
                print(output.read(), end='', flush=True)
                if status:
                    raise RuntimeError(f'worker failed ({status}); see {logpath}')
                return
            now = time.monotonic()
            if now - start >= timeout:
                raise TimeoutError(f'worker deadline exceeded; see {logpath}')
            if now >= heartbeat:
                progress(f'Worker active ({now - start:.0f}s elapsed); waiting for durable I/O. Ctrl+C cancels.')
                heartbeat = now + 5
            time.sleep(.1)


def save(path, value):
    temporary = path.with_suffix(path.suffix + '.new')
    with temporary.open('w') as f:
        json.dump(value, f, indent=2, sort_keys=True)
        f.write('\n')
        f.flush()
        os.fsync(f.fileno())
    temporary.replace(path)
    sync_dir(path.parent)


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def record(path, value):
    with path.open('a') as f:
        f.write(json.dumps(dict(time=time.time(), **value), sort_keys=True) + '\n')
        f.flush()
        os.fsync(f.fileno())


class DeviceNotReady(RuntimeError):
    pass


def identify():
    data = json.loads(run(['lsblk', '--json', '--bytes', '--nodeps', '--output',
                           'PATH,SERIAL,SIZE,TYPE,TRAN,RM'], capture_output=True, text=True).stdout)
    candidates = [d for d in data['blockdevices'] if d.get('serial') == SERIAL]
    if not candidates:
        raise DeviceNotReady(f'USB serial {SERIAL} has not enumerated yet')
    if len(candidates) != 1:
        raise RuntimeError(f'expected exactly one disk with serial {SERIAL}, found {len(candidates)}')
    d = candidates[0]
    if d['type'] != 'disk' or d['tran'] != 'usb' or not d['rm']:
        raise RuntimeError('target must be a whole removable USB disk')
    if not 115 * 1024**3 <= int(d['size']) <= 117 * 1024**3:
        raise RuntimeError('target capacity is outside the agreed 116.1 GiB range')
    sysfs = Path('/sys/class/block') / Path(d['path']).name
    d['diskseq'] = int((sysfs / 'diskseq').read_text())
    d['sysfs'] = str(sysfs)
    return d


def gone(d):
    try:
        return int((Path(d['sysfs']) / 'diskseq').read_text()) != d['diskseq']
    except FileNotFoundError:
        return True


def sectors_written(d):
    return int((Path(d['sysfs']) / 'stat').read_text().split()[6])


def snapshot(path):
    result = {}
    for p in sorted(path.rglob('*')):
        st = p.stat()
        name = str(p.relative_to(path))
        if p.is_dir():
            result[name] = {'kind': 'directory'}
        elif p.is_file():
            result[name] = {'kind': 'file', 'size': st.st_size,
                            'sha256': hashlib.sha256(p.read_bytes()).hexdigest(),
                            'executable': bool(st.st_mode & 0o111), 'mtime': int(st.st_mtime)}
        else:
            raise AssertionError(f'unexpected file type: {name}')
    return result


def write_sync(path, content):
    with path.open('wb') as f:
        f.write(content)
        f.flush()
        os.fsync(f.fileno())
    sync_dir(path.parent)


def nano_save(path):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ['TERM'] = 'xterm'
        os.execvp('nano', ['nano', '--ignorercfiles', str(path)])
    reaped = False
    try:
        for keys in [b'Actual nano save on XFFS.\n', b'\x0f', b'\r', b'\x18']:
            deadline = time.monotonic() + .5
            while time.monotonic() < deadline:
                ready, _, _ = select.select([fd], [], [], .05)
                if ready:
                    os.read(fd, 65536)
            os.write(fd, keys)
        deadline = time.monotonic() + 10
        while True:
            ended, status = os.waitpid(pid, os.WNOHANG)
            if ended:
                reaped = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            if time.monotonic() > deadline:
                raise TimeoutError('nano did not exit')
            ready, _, _ = select.select([fd], [], [], .1)
            if ready:
                try:
                    os.read(fd, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
        assert path.read_bytes() == b'Actual nano save on XFFS.\n'
        with path.open('rb') as f:
            os.fsync(f.fileno())
    finally:
        os.close(fd)
        if not reaped:
            try:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            except ProcessLookupError:
                pass


def workload(mount, report, fresh=False):
    if (report / 'baseline.json').exists():
        raise RuntimeError('a completed baseline already exists; verify it instead of replacing it')
    if not fresh and ((mount / 'acceptance-work').exists() or (mount / 'persist').exists()):
        raise RuntimeError('prior workload files exist; use recover and inspect them before another workload')
    suffix = '-' + uuid.uuid4().hex if fresh else ''
    before = snapshot(mount) if fresh else {}
    if fresh:
        save(report / ('workload-attempt' + suffix + '.json'), {'preserved_files': before})
        progress('Preserving existing files; using new test directories with suffix ' + suffix)
    progress('Testing directories, writes, append, rename, truncate and sparse files...')
    baseline = os.statvfs(mount)
    work = mount / ('acceptance-work' + suffix)
    work.mkdir()
    (work / 'nested').mkdir()
    p = work / 'nested/original'
    write_sync(p, b'first')
    with p.open('ab') as f:
        f.write(b' appended')
        f.flush()
        os.fsync(f.fileno())
    shutil.copyfile(p, work / 'copy')
    p.rename(work / 'renamed')
    os.replace(work / 'copy', work / 'renamed')
    assert (work / 'renamed').read_bytes() == b'first appended'
    with (work / 'renamed').open('r+b') as f:
        f.truncate(3)
    assert (work / 'renamed').read_bytes() == b'fir'
    sparse = work / 'sparse'
    with sparse.open('wb') as f:
        f.seek(1024 * 1024)
        f.write(b'end')
        f.flush()
        os.fsync(f.fileno())
    with sparse.open('rb') as f:
        assert f.read(4096) == bytes(4096)
    assert sparse.stat().st_blocks * 512 < sparse.stat().st_size
    exe = work / 'run.sh'
    write_sync(exe, b'#!/bin/sh\nprintf usb-execution\n')
    exe.chmod(0o755)
    assert run([exe], capture_output=True).stdout == b'usb-execution'
    os.utime(exe, (1700000000, 1700000010))
    assert int(exe.stat().st_atime) == 1700000000
    assert int(exe.stat().st_mtime) == 1700000010
    with (work / 'renamed').open('rb') as f:
        (work / 'renamed').unlink()
        assert f.read() == b'fir'
    progress('Testing a real nano save...')
    nano_save(work / 'nano.txt')
    shutil.rmtree(work)
    sync_dir(mount)
    after = os.statvfs(mount)
    assert (after.f_bfree, after.f_ffree) == (baseline.f_bfree, baseline.f_ffree)
    progress('Free-space restoration passed; preparing persistent files...')
    persist = mount / ('persist' + suffix)
    persist.mkdir()
    write_sync(persist / 'anchor', b'acknowledged immutable USB data\n' * 1024)
    data = bytes(range(256)) * (8 * 1024 * 1024 // 256)
    start = time.monotonic()
    progress('Writing 8 MiB to USB with durability barriers; this can take minutes...')
    with (persist / 'throughput.bin').open('xb') as f:
        for offset in range(0, len(data), 1024 * 1024):
            f.write(data[offset:offset + 1024 * 1024])
            f.flush()
            os.fsync(f.fileno())
            progress(f'Durable write: {offset // (1024 * 1024) + 1}/8 MiB')
    sync_dir(persist)
    elapsed = time.monotonic() - start
    save(report / 'performance.json', {'path': str((persist / 'throughput.bin').relative_to(mount)),
         'bytes': len(data), 'write_fsync_seconds': elapsed,
         'write_fsync_mib_s': len(data) / 1024**2 / elapsed,
         'scope': 'USB + buffered Linux backend + FUSE + transaction flushes; not raw media bandwidth'})
    finished = snapshot(mount)
    for name, entry in before.items():
        assert finished.get(name) == entry, f'pre-existing file changed: {name}'
    save(report / 'baseline.json', finished)
    save(report / 'workload.json', {'passed': True, 'nano': 'real PTY save', 'free_space_restored': True})


def payload(kind, n, version='new'):
    unit = f'XFFS {kind} {n:06d} {version}\n'.encode()
    return (unit * (65536 // len(unit) + 1))[:65536]


def cow_versions(n):
    # Alternate full-block and unaligned partial-block updates across three
    # blocks. Patterns identify the file/version and preserve untouched bytes.
    old = hashlib.shake_256(f'XFFS cow {n}'.encode()).digest(3 * 4096)
    offset = 0 if n % 2 == 0 else 137
    length = 3 * 4096 if n % 2 == 0 else 8192
    replacement = bytes(b ^ 255 for b in old[offset:offset + length])
    new = old[:offset] + replacement + old[offset + len(replacement):]
    return old, new, offset, replacement


def verify_cow(actual, n, phase, overwriting):
    old, new, _, _ = cow_versions(n)
    if phase == 'done':
        assert actual == new, f'acknowledged COW overwrite lost: {n}'
    elif overwriting:
        assert phase == 'old', f'COW overwrite without durable old data: {n}'
        # A multi-block request may publish only a prefix of block replacements.
        assert actual in [new[:cut] + old[cut:] for cut in range(0, len(old) + 1, 4096)], (
            f'torn or non-prefix COW overwrite: {n}')
    elif phase == 'old':
        assert actual == old, f'acknowledged COW initialization lost: {n}'
    else:
        assert actual is None or old.startswith(actual), f'invalid COW initialization: {n}'


def cow_state(slot, generation):
    base, _, offset, replacement = cow_versions(slot)
    base = bytes(b & 127 for b in base)
    if generation == 0:
        return base
    digest = hashlib.shake_256(f'XFFS cow {slot} generation {generation}'.encode()).digest(len(replacement))
    # Adjacent generations differ in every updated byte; hashes distinguish
    # generations beyond a simple old/new toggle and detect stale reused data.
    data = bytes((b & 127) | ((generation % 2) << 7) for b in digest)
    return base[:offset] + data + base[offset + len(data):]


def cow_worker(mount, report, iterations=None):
    # A unique folder allows evidence-preserving retries without changing the
    # previous attempt's files, which are now part of the baseline manifest.
    folder = mount / ('trial-cow-' + uuid.uuid4().hex)
    log = report / 'trial-cow.jsonl'
    with log.open('x'):
        pass
    record(log, {'event': 'cow-start', 'version': 2, 'folder': folder.name})
    folder.mkdir()
    sync_dir(mount)
    for slot in range(2):
        record(log, {'event': 'initialize', 'slot': slot})
        write_sync(folder / f'{slot:06d}', cow_state(slot, 0))
        sync_dir(folder)
        record(log, {'event': 'initialized', 'slot': slot})
    # No creation, truncation, or rename occurs after readiness.
    with (folder / '000000').open('r+b', buffering=0) as full, (
            folder / '000001').open('r+b', buffering=0) as partial:
        for n in range(100000 if iterations is None else iterations):
            slot, generation = n % 2, n // 2 + 1
            _, _, offset, replacement = cow_versions(slot)
            target = cow_state(slot, generation)
            record(log, {'event': 'cow-intent', 'n': n, 'slot': slot,
                         'generation': generation})
            if n == 0:
                save(report / 'ready-cow.json', {'ready': True, 'version': 2})
            stage = 'write'
            try:
                f = (full, partial)[slot]
                f.seek(offset)
                remaining = memoryview(target[offset:offset + len(replacement)])
                while remaining:
                    written = f.write(remaining)
                    if not written:
                        raise OSError('overwrite made no progress')
                    remaining = remaining[written:]
                # Distinguish an interrupted data-write attempt from an error
                # in fsync or a gap after the write returned.
                record(log, {'event': 'cow-write-returned', 'n': n})
                stage = 'fsync'
                os.fsync(f.fileno())
                record(log, {'event': 'cow-ack', 'n': n})
            except OSError as error:
                record(log, {'event': 'cow-interrupted', 'n': n,
                             'stage': stage, 'error': str(error)})
                return
    if iterations is None:
        raise RuntimeError('trial workload exhausted without unplugging')


def verify_cow_repeated(mount, events):
    start = events[0]
    assert start['version'] == 2, 'unsupported COW evidence version'
    name = start['folder']
    assert name.startswith('trial-cow-') and Path(name).name == name, 'invalid COW folder'
    folder = mount / name
    assert folder.is_dir()
    expected = {}
    initialized = set()
    initializing = set()
    pending = None
    returned = False
    interrupted_write = False
    count = 0
    for event in events[1:]:
        kind = event['event']
        if kind == 'initialize':
            slot = event['slot']
            assert slot in (0, 1) and slot not in initializing
            initializing.add(slot)
        elif kind == 'initialized':
            slot = event['slot']
            assert slot in initializing and slot not in initialized
            initialized.add(slot)
            expected[slot] = 0
        elif kind == 'cow-intent':
            assert initialized == {0, 1} and pending is None
            assert event['n'] == count and event['slot'] == count % 2
            assert event['generation'] == expected[event['slot']] + 1
            pending = event
            returned = False
            interrupted_write = False
            count += 1
        elif kind == 'cow-write-returned':
            assert pending and event['n'] == pending['n'] and not returned
            returned = True
        elif kind == 'cow-ack':
            assert pending and event['n'] == pending['n'] and returned
            expected[pending['slot']] = pending['generation']
            pending = None
        elif kind == 'cow-interrupted':
            assert pending and event['n'] == pending['n']
            assert event['stage'] in ('write', 'fsync')
            assert (event['stage'] == 'fsync') == returned
            interrupted_write = event['stage'] == 'write'
        else:
            raise AssertionError('unknown COW evidence event')
    allowed = {f'{slot:06d}' for slot in initializing}
    assert {p.name for p in folder.iterdir()} <= allowed, 'unexpected COW namespace'
    outcome = None
    for slot in initializing:
        path = folder / f'{slot:06d}'
        actual = path.read_bytes() if path.exists() else None
        if slot not in initialized:
            old = cow_state(slot, 0)
            assert actual is None or old.startswith(actual), 'invalid COW initialization'
            continue
        old = cow_state(slot, expected[slot])
        if pending and slot == pending['slot']:
            new = cow_state(slot, pending['generation'])
            candidates = [new[:cut] + old[cut:] for cut in range(0, len(old) + 1, 4096)]
            assert actual in candidates, 'torn or non-prefix COW overwrite'
            outcome = ('old' if actual == old else 'new' if actual == new else 'prefix')
        else:
            assert actual == old, 'acknowledged COW data lost'
    return {
        'version': 2, 'data_valid': True,
        'acknowledged_overwrites': count - int(pending is not None),
        'unacknowledged_overwrite': pending is not None,
        'write_error_observed': interrupted_write,
        'recovered_outcome': outcome,
        # A prefix is direct evidence of interrupted replacement publication.
        # Otherwise require an error during data writing, not merely missing
        # host acknowledgement or removal in the fsync/between-write gap.
        'overwrite_interruption_observed': interrupted_write or outcome == 'prefix',
    }


def cow_coverage_passed(report):
    path = report / 'cow-coverage.json'
    return path.exists() and json.loads(path.read_text()).get('overwrite_interruption_observed') is True


def archive_cow_attempt(report):
    if not any((report / name).exists() for name in ('passed-cow.json', 'recovered-cow.json')):
        raise RuntimeError('verify/recover the previous COW attempt before retrying')
    archive = report / ('cow-history-' + uuid.uuid4().hex)
    archive.mkdir()
    for name in ('trial-cow.jsonl', 'ready-cow.json', 'passed-cow.json',
                 'recovered-cow.json', 'cow-coverage.json', 'final.json'):
        path = report / name
        if path.exists():
            path.rename(archive / name)
    sync_dir(archive)
    sync_dir(report)
    record(report / 'events.jsonl', {'event': 'cow-retry', 'archive': archive.name})


def trial_worker(mount, report, kind, iterations=None):
    if kind == 'cow':
        return cow_worker(mount, report, iterations)
    folder = mount / ('trial-' + kind)
    folder.mkdir()
    sync_dir(mount)
    log = report / ('trial-' + kind + '.jsonl')
    # Exclusive creation prevents accidental reuse of trial evidence.
    with log.open('x'):
        pass
    for n in range(100000 if iterations is None else iterations):
        path = folder / f'{n:06d}'
        content = payload(kind, n)
        record(log, {'event': 'intent', 'n': n, 'kind': kind,
                     'sha256': hashlib.sha256(content).hexdigest(), 'bytes': len(content)})
        if n == 0 and kind != 'cow':
            save(report / ('ready-' + kind + '.json'), {'ready': True})
        def ack(phase):
            record(log, {'event': 'ack', 'n': n, 'phase': phase})
        try:
            if kind == 'create':
                write_sync(path, content[:32768])
                with path.open('ab') as f:
                    f.write(content[32768:])
                    f.flush()
                    os.fsync(f.fileno())
                sync_dir(folder)
                ack('done')
            elif kind == 'replace':
                write_sync(path, payload(kind, n, 'old'))
                ack('old')
                temporary = path.with_suffix('.tmp')
                write_sync(temporary, content)
                ack('new')
                os.replace(temporary, path)
                sync_dir(folder)
                ack('done')
            else:
                write_sync(path, content)
                ack('ready')
                with path.open('r+b') as f:
                    f.truncate(4096)
                    f.flush()
                    os.fsync(f.fileno())
                ack('truncated')
                path.unlink()
                sync_dir(folder)
                ack('done')
            time.sleep(.02)
        except OSError as error:
            record(log, {'event': 'interrupted', 'n': n, 'error': str(error)})
            return
    if iterations is None:
        raise RuntimeError('trial workload exhausted without unplugging')


def verify_trial(mount, report, kind):
    events = [json.loads(line) for line in (report / ('trial-' + kind + '.jsonl')).read_text().splitlines()]
    if kind == 'cow' and events and events[0]['event'] == 'cow-start':
        save(report / 'cow-coverage.json', verify_cow_repeated(mount, events))
        return
    if kind == 'cow':
        save(report / 'cow-coverage.json', {
            'version': 1, 'overwrite_interruption_observed': False,
            'reason': 'legacy trial: no direct overwrite-interruption evidence'})
    intentions = {e['n']: e for e in events if e['event'] == 'intent'}
    phases = {e['n']: e['phase'] for e in events if e['event'] == 'ack'}
    overwrites = {e['n'] for e in events if e['event'] == 'overwrite'}
    folder = mount / ('trial-' + kind)
    assert folder.is_dir()
    allowed = set()
    for n in intentions:
        path = folder / f'{n:06d}'
        allowed.add(path.name)
        content = payload(kind, n)
        phase = phases.get(n)
        actual = path.read_bytes() if path.exists() else None
        if kind == 'cow':
            verify_cow(actual, n, phase, n in overwrites)
        elif kind == 'create':
            if phase == 'done':
                assert actual == content, f'acknowledged create lost: {n}'
            else:
                assert actual is None or content.startswith(actual), f'invalid interrupted append: {n}'
        elif kind == 'cleanup':
            if phase == 'done':
                assert actual is None, f'acknowledged deletion lost: {n}'
            elif phase == 'truncated':
                assert actual in (None, content[:4096])
            elif phase == 'ready':
                assert actual in (content, content[:4096])
            else:
                assert actual is None or content.startswith(actual)
        else:
            temporary = path.with_suffix('.tmp')
            allowed.add(temporary.name)
            temp = temporary.read_bytes() if temporary.exists() else None
            old = payload(kind, n, 'old')
            if phase == 'done':
                assert actual == content and temp is None, f'acknowledged replacement lost: {n}'
            elif phase == 'new':
                assert (actual, temp) in ((old, content), (content, None))
            elif phase == 'old':
                assert actual == old and (temp is None or content.startswith(temp))
            else:
                assert (actual is None or old.startswith(actual)) and temp is None
    assert {p.name for p in folder.iterdir()} <= allowed, 'unexpected trial namespace'


def verify(mount, report, trial=None, cold=False):
    expected = json.loads((report / 'baseline.json').read_text())
    if cold:
        perf = json.loads((report / 'performance.json').read_text())
        start = time.monotonic()
        data = (mount / perf.get('path', 'persist/throughput.bin')).read_bytes()
        elapsed = time.monotonic() - start
        perf = json.loads((report / 'performance.json').read_text())
        perf.update(reconnected_read_seconds=elapsed, reconnected_read_mib_s=len(data) / 1024**2 / elapsed)
        save(report / 'performance.json', perf)
    actual = snapshot(mount)
    for name, entry in expected.items():
        assert actual.get(name) == entry, f'acknowledged manifest mismatch: {name}'
    extras = set(actual) - set(expected)
    if trial:
        prefix = 'trial-' + trial
        if trial == 'cow':
            first = json.loads((report / 'trial-cow.jsonl').read_text().splitlines()[0])
            if first['event'] == 'cow-start':
                prefix = first['folder']
        assert all(name == prefix or name.startswith(prefix + '/') for name in extras)
        verify_trial(mount, report, trial)
    else:
        assert not extras, f'unexpected names: {extras}'
    save(report / 'verified.json', {'time': time.time(), 'trial': trial, 'passed': True})


def is_mounted(path):
    return any(line.split()[4] == str(path) for line in Path('/proc/self/mountinfo').read_text().splitlines())


class Harness:
    def __init__(self, report, device):
        self.uid = int(os.environ.get('SUDO_UID', '0'))
        self.gid = int(os.environ.get('SUDO_GID', '0'))
        if os.geteuid() != 0 or not self.uid or not self.gid:
            raise RuntimeError('UNMET PREREQUISITE: run via sudo from a non-root account')
        for tool in ['fusermount3', 'lsblk', 'blockdev', 'nano']:
            if not shutil.which(tool):
                raise RuntimeError('UNMET PREREQUISITE: ' + tool)
        if not Path('/dev/fuse').exists():
            raise RuntimeError('UNMET PREREQUISITE: /dev/fuse')
        self.report = report.resolve()
        ancestor = self.report
        while not ancestor.exists():
            ancestor = ancestor.parent
        sysfs = Path(device['sysfs'])
        ids = {(sysfs / 'dev').read_text().strip()}
        ids.update((p / 'dev').read_text().strip() for p in sysfs.iterdir()
                   if (p / 'partition').exists())
        hostdev = ancestor.stat().st_dev
        if f'{os.major(hostdev)}:{os.minor(hostdev)}' in ids:
            raise RuntimeError('evidence cannot be stored on the target disk')
        # Validate the host location before creating any evidence directory.
        for line in Path('/proc/self/mountinfo').read_text().splitlines():
            fields = line.split()
            filesystem = line.split(' - ')[1].split()[0]
            mount = Path(fields[4])
            if self.report == mount or mount in self.report.parents:
                if filesystem.startswith('fuse'):
                    raise RuntimeError('report directory must be on a host filesystem')
                source = line.split(' - ')[1].split()[1]
                if source.startswith('/dev/') and Path(source).exists():
                    dev = Path(source).stat().st_rdev
                    if f'{os.major(dev)}:{os.minor(dev)}' in ids:
                        raise RuntimeError('evidence cannot be stored on the target disk')
        if not self.report.exists():
            self.report.mkdir(parents=True)
            os.chown(self.report, self.uid, self.gid)
        session = self.report / 'session.json'
        if session.exists():
            identity = json.loads(session.read_text())
            if identity != {'harness': 'xffs-usb-acceptance-v1', 'serial': SERIAL}:
                raise RuntimeError('evidence session identity mismatch')
        else:
            if any(self.report.iterdir()):
                raise RuntimeError('use an empty evidence directory; existing files will not be replaced')
            save(session, {'harness': 'xffs-usb-acceptance-v1', 'serial': SERIAL})
        self.events = self.report / 'events.jsonl'
        self.user = {'user': self.uid, 'group': self.gid, 'extra_groups': []}

    def command(self, args, label, required=True, retry_lock=False):
        progress(f'{label}: running...')
        deadline = time.monotonic() + 15
        attempt = 0
        while True:
            attempt += 1
            result = subprocess.run([str(x) for x in args], capture_output=True, text=True, timeout=120)
            stamp = str(time.time_ns())
            (self.report / (stamp + '-' + label + '.log')).write_text(result.stdout + result.stderr)
            record(self.events, {'command': [str(x) for x in args], 'exit': result.returncode,
                                 'label': label, 'attempt': attempt})
            # Retry only read-only commands that failed to acquire the claim.
            # Never retry formatting, recovery, or a filesystem/identity error.
            busy = result.returncode != 0 and result.stderr.strip() == 'Error: LockContention'
            remaining = deadline - time.monotonic()
            if retry_lock and busy and remaining > 0:
                progress(f'{label}: device temporarily busy; retrying exclusive claim...')
                time.sleep(min(1, remaining))
                continue
            if required and result.returncode:
                raise RuntimeError(f'{label} failed: {result.stderr}; evidence retained in {self.report}')
            if not result.returncode and label == 'check':
                progress(result.stdout.strip())
            return result

    def identity_args(self, d):
        return ['--expect-serial', SERIAL, '--expect-disk-sequence', str(d['diskseq'])]

    def check(self, d, inspect=False):
        if inspect:
            self.command([BIN / 'xffs-inspect', d['path'], '--device'] + self.identity_args(d), 'raw-inspect', required=False, retry_lock=True)
        self.command([BIN / 'xffs-check', d['path'], '--device', '--memory-mib', '512'] + self.identity_args(d), 'check', retry_lock=True)

    def worker(self, mode, mount, kind=None, wait=True):
        args = [sys.executable, '-u', __file__, '_worker', mode, str(mount), str(self.report)]
        if kind:
            args.append(kind)
        logpath = self.report / (str(time.time_ns()) + '-worker.log')
        with logpath.open('w') as log:
            proc = subprocess.Popen(args, stdout=log, stderr=log, start_new_session=True, **self.user)
        if not wait:
            return proc
        progress(f'{mode}: started; log {logpath.name}')
        try:
            wait_worker(proc, logpath)
        finally:
            stop_worker(proc)

    @contextmanager
    def mount(self, d, writable=False, abrupt=False):
        tmp = Path(tempfile.mkdtemp(prefix='xffs-usb-mount-'))
        try:
            os.chown(tmp, self.uid, self.gid)
            mount = tmp / 'mount'
            mount.mkdir()
            os.chown(mount, self.uid, self.gid)
            args = [BIN / 'mount-xffs', d['path'], mount, '--device', '--memory-mib', '512',
                    '--uid', str(self.uid), '--gid', str(self.gid)] + self.identity_args(d)
            if writable:
                args.append('--rw')
            with (self.report / (str(time.time_ns()) + '-mount.log')).open('w') as log:
                proc = subprocess.Popen(args, stdout=log, stderr=log, start_new_session=True)
                progress('Opening filesystem and completing recovery before mount...')
                try:
                    deadline = time.monotonic() + 30
                    while not is_mounted(mount):
                        if proc.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError('mount failed or timed out; see host mount log')
                        time.sleep(.05)
                    conflict = self.command([BIN / 'xffs-check', d['path'], '--device'], 'exclusion', required=False)
                    assert conflict.returncode and 'LockContention' in conflict.stderr, 'exclusive claim was not confirmed'
                    progress(f'Mounted at {mount}')
                    yield mount
                finally:
                    active_error = sys.exc_info()[1]
                    progress('Stopping workload and unmounting; please wait...')
                    try:
                        if is_mounted(mount):
                            run(['fusermount3', '-uz' if abrupt else '-u', mount], **self.user)
                    finally:
                        try:
                            proc.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            proc.kill()
                            proc.wait(timeout=5)
                    if not abrupt and proc.returncode:
                        message = f'unclean mount service exit: {proc.returncode}'
                        if active_error is None:
                            raise RuntimeError(message)
                        progress(message)
                    elif not is_mounted(mount):
                        progress('Unmounted; service exited.')
        finally:
            # Never recursively remove a directory which may still contain a mount.
            mount = tmp / 'mount'
            if is_mounted(mount):
                raise RuntimeError(f'cleanup incomplete; mountpoint retained at {mount}')
            if mount.exists():
                mount.rmdir()
            tmp.rmdir()

    def readonly(self, d, trial=None, cold=False):
        before = sectors_written(d)
        with self.mount(d) as mount:
            self.worker('verify-trial' if trial else ('verify-cold' if cold else 'verify'), mount, trial)
            self.worker('readonly', mount)
        assert sectors_written(d) == before, 'read-only service caused block-device writes'


def wait_for_reconnect(old, timeout=60):
    deadline = time.monotonic() + timeout
    notice = 0
    while True:
        try:
            new = identify()
        except (DeviceNotReady, FileNotFoundError):
            new = None
        if new is not None:
            if new['size'] != old['size']:
                raise RuntimeError('reconnect capacity mismatch; refusing device')
            if new['diskseq'] != old['diskseq']:
                progress('Matching USB enumerated; continuing verification.')
                return new
        now = time.monotonic()
        if now >= deadline:
            raise TimeoutError('USB did not enumerate within 60 seconds; evidence retained. '
                               'For an interrupted trial, use resume-trial KIND after reconnecting.')
        if now >= notice:
            progress('Waiting for the matching USB to enumerate (up to 60 seconds)...')
            notice = now + 5
        time.sleep(min(.5, deadline - now))


def observed_removal(report, kind):
    if (report / ('passed-' + kind + '.json')).exists():
        raise RuntimeError('this trial already passed; do not repeat it')
    if not (report / ('trial-' + kind + '.jsonl')).exists():
        raise RuntimeError('no trial intentions exist to resume')
    events = [json.loads(line) for line in (report / 'events.jsonl').read_text().splitlines()]
    if kind == 'cow':
        retries = [i for i, e in enumerate(events) if e.get('event') == 'cow-retry']
        if retries:
            events = events[retries[-1] + 1:]
    removals = [e for e in events if e.get('event') == 'removal-observed' and e.get('kind') == kind]
    if not removals or removals[-1]['device']['serial'] != SERIAL:
        raise RuntimeError('no recorded removal for this trial; use verify instead')
    return removals[-1]['device']


def complete_trial(h, d, kind):
    h.check(d, inspect=True)
    h.readonly(d, trial=kind)
    with h.mount(d, writable=True) as mount:
        h.worker('verify-trial', mount, kind)
        h.worker('snapshot', mount)
    h.check(d)
    if kind == 'cow' and not cow_coverage_passed(h.report):
        save(h.report / 'recovered-cow.json', {'recovered': True, 'manual_trial_pass': False})
        save(h.report / 'final.json', {'check_passed': True, 'cleanly_unmounted': True,
             'acceptance_complete': False, 'pending': ['passed-cow.json']})
        raise RuntimeError('COW data valid, but no overwrite interruption observed; use trial cow --retry')
    save(h.report / ('passed-' + kind + '.json'), {'passed': True, 'diskseq': d['diskseq']})


def reconnect(old):
    input('Physically unplug the selected USB drive now, then press Enter: ')
    if not gone(old):
        raise RuntimeError('removal not observed; unplug trial is NOT a pass')
    input('Reconnect the USB drive, then press Enter (enumeration will be awaited): ')
    return wait_for_reconnect(old)


def worker_main():
    mode, mount, report = sys.argv[2:5]
    mount, report = Path(mount), Path(report)
    kind = sys.argv[5] if len(sys.argv) > 5 else None
    if mode in ('workload', 'resume-workload'):
        workload(mount, report, fresh=mode == 'resume-workload')
    elif mode == 'trial':
        trial_worker(mount, report, kind)
    elif mode == 'inventory':
        progress('Recording current files without changing or deleting them...')
        save(report / (str(time.time_ns()) + '-recovery-inventory.json'), snapshot(mount))
    elif mode == 'snapshot':
        save(report / 'baseline.json', snapshot(mount))
    elif mode == 'readonly':
        try:
            (mount / 'forbidden-ro-write').write_bytes(b'no')
        except OSError as error:
            assert error.errno == errno.EROFS
        else:
            raise AssertionError('read-only mount permitted writing')
    else:
        verify(mount, report, kind, cold=mode == 'verify-cold')


def finalize_format(h, d):
    attempt = json.loads((h.report / 'format-started.json').read_text())
    if attempt['device']['serial'] != SERIAL or attempt['device']['size'] != d['size']:
        raise RuntimeError('original format identity does not match this device')
    filesystem_uuid = str(uuid.UUID(attempt['uuid']))
    command = [BIN / 'xffs-verify-format', d['path'], '--expect-serial', SERIAL,
               '--expect-disk-sequence', str(d['diskseq']), '--expect-bytes', str(d['size']),
               '--expect-uuid', filesystem_uuid, '--expect-inodes', '65536']
    h.command(command, 'verify-existing-format', retry_lock=True)
    # Refresh only the in-memory kernel partition view; never invoke mkfs.
    if identify() != d:
        raise RuntimeError('identity changed before partition refresh')
    h.command(['blockdev', '--rereadpt', d['path']], 'partition-refresh')
    h.command(['udevadm', 'settle', '--timeout=15'], 'settle-partitions')
    if identify() != d:
        raise RuntimeError('identity changed after partition refresh')
    if any((p / 'partition').exists() for p in Path(d['sysfs']).iterdir()):
        raise RuntimeError('kernel still exposes partitions; format receipt not written')
    h.command(command, 'verify-existing-format', retry_lock=True)
    receipt = h.report / 'formatted.json'
    if not receipt.exists():
        save(receipt, {'passed': True, 'device': d, 'uuid': filesystem_uuid,
             'verification': 'existing format verified later; partition view refreshed',
             'original_format_command_succeeded': False, 'time': time.time()})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expect-serial', required=True, choices=[SERIAL])
    parser.add_argument('--report-dir', type=Path, required=True)
    sub = parser.add_subparsers(dest='action', required=True)
    sub.add_parser('identify')
    fmt = sub.add_parser('format')
    fmt.add_argument('--erase', action='store_true', required=True)
    for action in ['workload', 'reconnect', 'verify', 'finish', 'accept', 'recover', 'check', 'resume-workload', 'finalize-report']:
        sub.add_parser(action)
    trial = sub.add_parser('trial')
    trial.add_argument('kind', choices=TRIALS)
    trial.add_argument('--retry', action='store_true', help='archive a recovered COW attempt and retry without formatting')
    resume = sub.add_parser('resume-trial')
    resume.add_argument('kind', choices=TRIALS)
    args = parser.parse_args()
    if args.action == 'trial' and args.retry and args.kind != 'cow':
        parser.error('--retry is only supported for trial cow')
    if args.action == 'accept':
        # Reusable acceptance never invokes formatting. Each child has its own
        # deadline and records evidence before the next phase starts.
        signal.alarm(0)
        common = [sys.executable, __file__, '--expect-serial', SERIAL,
                  '--report-dir', args.report_dir]
        for step in [['workload'], ['reconnect'], ['trial', 'create'],
                     ['trial', 'replace'], ['trial', 'cleanup'], ['trial', 'cow'], ['finish']]:
            run(common + step, timeout=620)
        return
    if args.action == 'resume-trial':
        old = observed_removal(args.report_dir, args.kind)
        d = wait_for_reconnect(old)
    else:
        d = identify()
    if args.action == 'identify':
        print(json.dumps(d, indent=2))
        return
    h = Harness(args.report_dir, d)
    record(h.events, {'event': 'start', 'action': args.action, 'device': d})
    if args.action == 'finalize-report':
        finalize_format(h, d)
        args.action = 'finish'
    if args.action == 'format':
        marker = h.report / 'format-started.json'
        if marker.exists():
            raise RuntimeError('this report already records a format attempt; refusing to reformat')
        save(marker, {'device': d, 'uuid': str(uuid.uuid4())})
        filesystem_uuid = json.loads(marker.read_text())['uuid']
        h.command([BIN / 'mkfs-xffs', d['path'], '--device', '--erase', '--expect-serial', SERIAL,
                   '--uuid', filesystem_uuid, '--inodes', '65536'], 'format')
        h.check(d)
        save(h.report / 'formatted.json', {'passed': True, 'device': d, 'uuid': filesystem_uuid})
    elif args.action == 'check':
        h.check(d)
    elif args.action == 'recover':
        h.check(d, inspect=True)
        with h.mount(d, writable=True) as mount:
            h.worker('inventory', mount)
        h.check(d)
        record(h.events, {'event': 'recovery-complete', 'acceptance_pass': False})
    elif args.action in ('workload', 'resume-workload'):
        h.check(d)
        with h.mount(d, writable=True) as mount:
            h.worker(args.action, mount)
        h.check(d)
        h.readonly(d)
    elif args.action == 'reconnect':
        h.check(d)
        d = reconnect(d)
        h.check(d, inspect=True)
        h.readonly(d, cold=True)
        with h.mount(d, writable=True) as mount:
            h.worker('verify', mount)
        h.check(d)
    elif args.action == 'resume-trial':
        complete_trial(h, d, args.kind)
    elif args.action == 'trial':
        kind = args.kind
        if (h.report / ('trial-' + kind + '.jsonl')).exists() and not args.retry:
            raise RuntimeError('trial evidence already exists; verify/recover it, never reformat')
        h.check(d)
        with h.mount(d, writable=True, abrupt=True) as mount:
            h.worker('verify', mount)
            if args.retry:
                archive_cow_attempt(h.report)
            worker = h.worker('trial', mount, kind, wait=False)
            try:
                ready = h.report / ('ready-' + kind + '.json')
                deadline = time.monotonic() + 15
                while not ready.exists():
                    if worker.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError('trial worker did not start')
                    time.sleep(.05)
                input(f'{kind} workload is active. Unplug the USB drive NOW, then press Enter: ')
                if not gone(d):
                    raise RuntimeError('removal not observed; trial is NOT a pass')
                record(h.events, {'event': 'removal-observed', 'kind': kind, 'device': d})
                try:
                    worker.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(worker.pid, signal.SIGKILL)
                    worker.wait(timeout=10)
            finally:
                if worker.poll() is None:
                    os.killpg(worker.pid, signal.SIGKILL)
                    worker.wait(timeout=10)
        input('Reconnect the USB drive, then press Enter (enumeration will be awaited): ')
        d = wait_for_reconnect(d)
        complete_trial(h, d, kind)
    else:
        h.check(d, inspect=True)
        # An interrupted trial can be verified/recovered without reformatting.
        pending = [k for k in TRIALS
                   if (h.report / ('trial-' + k + '.jsonl')).exists()
                   and not (h.report / ('passed-' + k + '.json')).exists()]
        if len(pending) > 1:
            raise RuntimeError('multiple incomplete trials require examination')
        kind = pending[0] if pending else None
        h.readonly(d, trial=kind)
        with h.mount(d, writable=True) as mount:
            h.worker('verify-trial' if kind else 'verify', mount, kind)
            if kind:
                h.worker('snapshot', mount)
        h.check(d)
        if kind:
            save(h.report / ('recovered-' + kind + '.json'), {'recovered': True,
                 'manual_trial_pass': False, 'reason': 'verify-only recovery does not establish observed unplug'})
        if args.action == 'finish':
            required = ['formatted.json', 'workload.json', 'passed-reconnect.json'] + ['passed-' + k + '.json' for k in TRIALS]
            missing = [p for p in required if not (h.report / p).exists()]
            if not cow_coverage_passed(h.report) and 'passed-cow.json' not in missing:
                missing.append('passed-cow.json')
            save(h.report / 'final.json', {'check_passed': True, 'cleanly_unmounted': True,
                 'acceptance_complete': not missing, 'pending': missing})
            if missing:
                progress('Filesystem checks passed; acceptance report is incomplete: ' + ', '.join(missing))
                progress('Preserve the evidence; do not reformat to fill a missing receipt.')
    if args.action == 'reconnect':
        save(h.report / 'passed-reconnect.json', {'passed': True, 'diskseq': d['diskseq']})
    record(h.events, {'event': 'complete', 'action': args.action, 'device': d})
    print(f'{args.action} finished; cleanly unmounted. Evidence: {h.report}')


if __name__ == '__main__':
    def timeout_handler(*_):
        raise TimeoutError('acceptance command deadline exceeded')
    signal.signal(signal.SIGTERM, timeout_handler)
    signal.signal(signal.SIGALRM, timeout_handler)
    signal.alarm(600)
    if len(sys.argv) > 1 and sys.argv[1] == '_worker':
        worker_main()
    else:
        try:
            main()
        except KeyboardInterrupt:
            print('Cancelled. Partial files and host evidence retained; use recover before retrying.', file=sys.stderr, flush=True)
            sys.exit(130)
