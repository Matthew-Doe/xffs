#!/usr/bin/env python3
"""Explicit Linux filesystem comparisons. Image mode never selects physical disks."""
import argparse
from contextlib import contextmanager
import csv
import errno
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import statistics
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = 1
WORKLOADS = ('bulk', 'durable', 'small', 'replace')
DEFAULTS = dict(bulk_bytes=8*1024**2, bulk_chunk=128*1024,
                durable_bytes=1024**2, durable_chunk=4096,
                small_count=100, small_bytes=4096, replace_count=20, replace_bytes=65536)
FORMATS = {'exfat': 'mkfs.exfat', 'fat32': 'mkfs.fat', 'ext4': 'mkfs.ext4', 'f2fs': 'mkfs.f2fs'}
KERNEL = {'exfat': 'exfat', 'fat32': 'vfat', 'ext4': 'ext4', 'f2fs': 'f2fs'}


class Prerequisite(RuntimeError):
    pass


class Unsupported(RuntimeError):
    pass


def save(path, value):
    temp = path.with_suffix(path.suffix + '.new')
    with temp.open('w') as f:
        json.dump(value, f, indent=2, sort_keys=True)
        f.write('\n')
        f.flush()
        os.fsync(f.fileno())
    temp.replace(path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def unescape(value):
    return re.sub(r'\\([0-7]{3})', lambda m: chr(int(m[1], 8)), value)


def mounts():
    result = []
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        left, right = line.split(' - ', 1)
        a, b = left.split(), right.split()
        result.append(dict(id=a[2], path=unescape(a[4]), options=a[5],
                           filesystem=b[0], source=unescape(b[1]), super_options=b[2]))
    return result


def mounted(path):
    return next((m for m in mounts() if m['path'] == str(Path(path).absolute())), None)


def stop(proc):
    # Kill the entire isolated group, including descendants of an exited leader.
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    proc.wait(timeout=5)


def sync(fd):
    try:
        os.fsync(fd)
    except OSError as e:
        if e.errno in (errno.EINVAL, errno.ENOSYS, errno.EOPNOTSUPP):
            raise Unsupported(f'fsync unsupported: {e}') from e
        raise


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        sync(fd)
    finally:
        os.close(fd)


def write_all(fd, data):
    view = memoryview(data)
    while view:
        n = os.write(fd, view)
        if n <= 0:
            raise OSError('write made no progress')
        view = view[n:]


def write_file(path, chunks, each=False, latencies=None):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        for chunk in chunks:
            start = time.monotonic()
            write_all(fd, chunk)
            if each:
                sync(fd)
            if latencies is not None:
                latencies.append(time.monotonic() - start)
        if not each:
            sync(fd)
    finally:
        os.close(fd)


def payload(size, seed=0):
    unit = bytes((i + seed) % 256 for i in range(256))
    return (unit * ((size + 255)//256))[:size]


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024**2), b''):
            h.update(chunk)
    return h.hexdigest()


def expected(kind, p):
    if kind in ('bulk', 'durable'):
        return {'data': payload(p[kind + '_bytes'])}
    if kind == 'small':
        return {f'{i:06d}': payload(p['small_bytes'], i) for i in range(p['small_count'])}
    return {'destination': payload(p['replace_bytes'], p['replace_count'])}


def verify(folder, kind, p):
    contents = expected(kind, p)
    if {x.name for x in folder.iterdir()} != set(contents):
        raise RuntimeError(f'{kind}: namespace mismatch')
    for name, data in contents.items():
        path = folder / name
        if not stat.S_ISREG(path.lstat().st_mode) or path.stat().st_size != len(data) or digest(path) != hashlib.sha256(data).hexdigest():
            raise RuntimeError(f'{kind}/{name}: content mismatch')


def worker(mount, kind, p):
    folder = mount / kind
    folder.mkdir()
    # All content and chunk views are prepared before the clock starts.
    data = expected(kind, p)
    latencies = []
    if kind in ('bulk', 'durable'):
        content = memoryview(data['data'])
        chunks = [content[i:i+p[kind+'_chunk']] for i in range(0, len(content), p[kind+'_chunk'])]
        count, total = len(chunks), len(content)
    elif kind == 'small':
        count, total = len(data), sum(map(len, data.values()))
    else:
        replacements = [payload(p['replace_bytes'], i+1) for i in range(p['replace_count'])]
        write_file(folder / 'destination', [payload(p['replace_bytes'])])
        sync_dir(folder)
        count, total = len(replacements), p['replace_count'] * p['replace_bytes']
    sync_dir(mount)
    start = time.monotonic()
    if kind in ('bulk', 'durable'):
        write_file(folder / 'data', chunks, kind == 'durable', latencies)
    elif kind == 'small':
        for name, content in data.items():
            op = time.monotonic()
            write_file(folder / name, [content])
            latencies.append(time.monotonic() - op)
        sync_dir(folder)
    else:
        for content in replacements:
            op = time.monotonic()
            write_file(folder / 'temporary', [content])
            os.replace(folder / 'temporary', folder / 'destination')
            sync_dir(folder)
            latencies.append(time.monotonic() - op)
    elapsed = time.monotonic() - start
    verify(folder, kind, p)
    v = os.statvfs(mount)
    return dict(status='measured', seconds=elapsed, bytes=total, operations=count,
                mib_s=total / 1024**2 / elapsed, ops_s=count / elapsed,
                latency_median_s=statistics.median(latencies),
                latency_p95_s=sorted(latencies)[max(0, math.ceil(.95*len(latencies))-1)],
                latency_seconds=latencies,
                latency_scope={'bulk': 'write call (excludes final fsync/close)',
                               'durable': 'write + fsync (excludes create/close)',
                               'small': 'create/write/fsync/close (excludes final directory fsync)',
                               'replace': 'create/write/fsync/close/rename/directory fsync'}[kind],
                capacity_bytes=v.f_blocks*v.f_frsize, free_bytes=v.f_bavail*v.f_frsize)


def worker_main(args):
    mode, mount, kind, config, output = args
    p = json.loads(Path(config).read_text())
    try:
        if mode == 'verify':
            verify(Path(mount)/kind, kind, p)
            result = dict(status='verified')
        else:
            result = worker(Path(mount), kind, p)
    except Unsupported as e:
        result = dict(status='unsupported', error=str(e))
    save(Path(output), result)


def identity(path, command):
    path = Path(path).resolve(strict=True)
    if not stat.S_ISBLK(path.stat().st_mode):
        raise RuntimeError('target is not a block device')
    data = json.loads(command(['lsblk', '--json', '--bytes', '--nodeps', '--output',
                              'PATH,SERIAL,SIZE,TYPE,TRAN,RM,LOG-SEC,MAJ:MIN', path]))
    if len(data['blockdevices']) != 1:
        raise RuntimeError('ambiguous device')
    d = data['blockdevices'][0]
    s = Path('/sys/class/block') / path.name
    if (s/'partition').exists():
        raise RuntimeError('target must be a whole disk')
    d['diskseq'] = int((s/'diskseq').read_text())
    d['sysfs'] = str(s.resolve())
    return d


def validate_identity(d, serial=None, original=None):
    if serial is not None and (d['type'] != 'disk' or d['tran'] != 'usb' or not d['rm'] or not serial or d['serial'] != serial):
        raise RuntimeError('target must be the explicitly named whole removable USB disk with matching serial')
    if int(d['size']) <= 0 or int(d['log-sec']) not in (512, 1024, 2048, 4096):
        raise RuntimeError('invalid capacity or logical sector size')
    if original is not None and d != original:
        raise RuntimeError('device identity changed; refusing further I/O')


def target_ids(d):
    s = Path(d['sysfs'])
    nodes = [s] + [p for p in s.iterdir() if (p/'partition').exists()]
    for node in nodes:
        if any((node/'holders').iterdir()):
            raise RuntimeError('target or partition has active holders')
    return {(node/'dev').read_text().strip() for node in nodes}


def backing_ids(device_id, seen=None):
    """Include stacked devices and loop backing files when checking evidence storage."""
    seen = set() if seen is None else seen
    if device_id in seen:
        return seen
    seen.add(device_id)
    s = Path('/sys/dev/block') / device_id
    if not s.exists():
        return seen
    for node in (s/'slaves').iterdir():
        backing_ids((node/'dev').read_text().strip(), seen)
    if (s/'partition').exists():
        backing_ids((s.resolve().parent/'dev').read_text().strip(), seen)
    if (s/'loop/backing_file').exists():
        backing = Path((s/'loop/backing_file').read_text().strip())
        dev = backing.stat().st_dev
        backing_ids(f'{os.major(dev)}:{os.minor(dev)}', seen)
    return seen


def refuse_busy(d, report, physical=True):
    ids = target_ids(d)
    table = mounts()
    if any(m['id'] in ids for m in table):
        raise RuntimeError('target or child is mounted')
    for line in Path('/proc/swaps').read_text().splitlines()[1:]:
        s = Path(unescape(line.split()[0])).stat()
        dev = s.st_rdev if stat.S_ISBLK(s.st_mode) else s.st_dev
        if backing_ids(f'{os.major(dev)}:{os.minor(dev)}') & ids:
            raise RuntimeError('target backs active swap')
    host = report.resolve()
    while not host.exists():
        host = host.parent
    dev = host.stat().st_dev
    if backing_ids(f'{os.major(dev)}:{os.minor(dev)}') & ids:
        raise RuntimeError('evidence is on target-dependent storage')
    # FUSE and overlay backing relationships are not reliably exposed in sysfs.
    containing = [m for m in table if host == Path(m['path']) or Path(m['path']) in host.parents]
    if physical and containing and max(containing, key=lambda m: len(m['path']))['filesystem'].startswith(('fuse', 'overlay')):
        raise RuntimeError('evidence requires an independently identifiable host filesystem')
    if physical:
        filesystem = max(containing, key=lambda m: len(m['path']))['filesystem'] if containing else ''
        if filesystem == 'btrfs' or (not (Path('/sys/dev/block')/f'{os.major(dev)}:{os.minor(dev)}').exists()
                                    and filesystem not in ('tmpfs', 'ramfs')):
            raise RuntimeError('cannot establish all evidence storage dependencies; choose a block-backed host filesystem or tmpfs')


def format_args(fs, target, args):
    if fs.startswith('xffs'):
        bindir = args.baseline_bin_dir if fs == 'xffs-0.0.1' else args.bin_dir
        cmd = [bindir/'mkfs-xffs', target, '--uuid', str(uuid.uuid4())]
        if args.action == 'image':
            cmd += ['--size-mib', str(args.image_mib)]
        else:
            cmd += ['--device', '--erase', '--expect-serial', args.expect_serial]
        return cmd
    extra = {'ext4': ['-F', '-E', 'nodiscard,lazy_itable_init=0,lazy_journal_init=0'],
             'fat32': ['-I', '-F', '32', '--mbr=n'],
             'exfat': ['-K', '-P', 'none'], 'f2fs': ['-f', '-t', '0']}[fs]
    return [FORMATS[fs], *extra, target]


class Runner:
    def __init__(self, args, report):
        self.args, self.report = args, report
        self.uid, self.gid = int(os.environ.get('SUDO_UID', '0')), int(os.environ.get('SUDO_GID', '0'))
        self.user = dict(user=self.uid, group=self.gid, extra_groups=[])
        self.sequence = 0
        self.device = None
        self.loop = None
        self.pending_image = None
        self.mountpoint = None
        self.service = None
        self.active_command = None

    def launch(self, argv, label, user=False):
        self.sequence += 1
        logpath = self.report / f'{self.sequence:04d}-{label}.log'
        argv = list(map(str, argv))
        with (self.report/'commands.jsonl').open('a') as f:
            f.write(json.dumps(dict(argv=argv, log=logpath.name, time=time.time()))+'\n')
            f.flush()
            os.fsync(f.fileno())
        with logpath.open('w') as log:
            proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT,
                                    start_new_session=True, **(self.user if user else {}))
        return proc, logpath

    def command(self, argv, label='command', user=False, required=True, timeout=120):
        proc, log = self.launch(argv, label, user)
        self.active_command = proc
        try:
            deadline = time.monotonic() + timeout
            heartbeat = time.monotonic() + 10
            while proc.poll() is None:
                now = time.monotonic()
                if now >= deadline:
                    raise TimeoutError(f'{label} timed out; see {log}')
                if now >= heartbeat:
                    print(f'{label}: still running; Ctrl+C cancels', flush=True)
                    heartbeat = now + 10
                time.sleep(.05)
            if required and proc.returncode:
                raise RuntimeError(f'{label} exited {proc.returncode}; see {log}')
            return log.read_text()
        finally:
            stop(proc)
            self.active_command = None

    def recheck(self):
        d = identity(self.device['path'], self.command)
        validate_identity(d, self.args.expect_serial if self.args.action == 'device' else None, self.device)
        refuse_busy(d, self.report, physical=self.args.action == 'device')

    def format(self, fs, sample):
        if self.args.action == 'device':
            self.recheck()
            target = self.device['path']
            self.command(['wipefs', '--all', '--force', target], 'wipe-signatures')
            self.command(['blockdev', '--rereadpt', target], 'refresh-partitions')
            self.command(['udevadm', 'settle', '--timeout=15'], 'settle')
            self.recheck()
            self.no_partitions()
        else:
            target = self.report / f"disk-{sample['repetition']}-{fs}.img"
            with target.open('xb') as f:
                f.truncate(self.args.image_mib * 1024**2)
            # XFFS's image formatter creates its own exclusive file.
            if fs.startswith('xffs'):
                target.unlink()
        argv = format_args(fs, target, self.args)
        sample['formatter_argv'] = list(map(str, argv))
        self.command(argv, 'format')  # Never retry formatting.
        if self.args.action == 'image':
            self.pending_image = target
            self.loop = self.command(['losetup', '--find', '--show', '--nooverlap', target], 'attach-loop').strip()
            self.device = identity(self.loop, self.command)
            sample['image'] = str(target)
        else:
            self.command(['blockdev', '--rereadpt', target], 'refresh-partitions')
            self.command(['udevadm', 'settle', '--timeout=15'], 'settle')
        self.recheck()
        self.no_partitions()
        sample['device'] = self.device

    def no_partitions(self):
        if any((p/'partition').exists() for p in Path(self.device['sysfs']).iterdir()):
            raise RuntimeError('kernel still exposes partitions')

    @contextmanager
    def mount(self, fs, readonly, sample):
        self.recheck()
        self.mountpoint = Path(tempfile.mkdtemp(prefix='xffs-benchmark-mount-'))
        os.chown(self.mountpoint, self.uid, self.gid)
        try:
            if fs.startswith('xffs'):
                bindir = self.args.baseline_bin_dir if fs == 'xffs-0.0.1' else self.args.bin_dir
                argv = [bindir/'mount-xffs', self.device['path'], self.mountpoint,
                        '--device', '--uid', self.uid, '--gid', self.gid,
                        '--expect-disk-sequence', self.device['diskseq']]
                if self.args.action == 'device':
                    argv += ['--expect-serial', self.args.expect_serial]
                if not readonly:
                    argv += ['--rw']
                self.service, _ = self.launch(argv, 'mount-fuse')
                deadline = time.monotonic() + 30
                while not mounted(self.mountpoint):
                    if self.service.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('FUSE mount exited or readiness timed out')
                    time.sleep(.05)
            else:
                opts = ['ro' if readonly else 'rw']
                if fs in ('fat32', 'exfat'):
                    opts += [f'uid={self.uid}', f'gid={self.gid}', 'umask=077']
                self.command(['mount', '-t', KERNEL[fs], '-o', ','.join(opts), self.device['path'], self.mountpoint], 'mount')
                if not readonly and fs in ('ext4', 'f2fs'):
                    os.chown(self.mountpoint, self.uid, self.gid)
            info = mounted(self.mountpoint)
            if not info or ('ro' if readonly else 'rw') not in info['options'].split(','):
                raise RuntimeError('mount not ready in requested mode')
            sample['mounts'].append(info)
            yield self.mountpoint
        finally:
            self.unmount()

    def unmount(self):
        if self.active_command is not None:
            raise RuntimeError('command group did not stop; refusing teardown beneath active I/O')
        if self.mountpoint is None:
            return
        if mounted(self.mountpoint):
            if self.service:
                self.command(['fusermount3', '-u', self.mountpoint], 'unmount', user=True)
            else:
                self.command(['umount', self.mountpoint], 'unmount')
        if mounted(self.mountpoint):
            raise RuntimeError(f'cleanup incomplete: {self.mountpoint} remains mounted')
        if self.service:
            try:
                self.service.wait(timeout=15)
                if self.service.returncode:
                    raise RuntimeError('unclean FUSE service exit')
            finally:
                stop(self.service)
                self.service = None
        self.mountpoint.rmdir()
        self.mountpoint = None

    def cleanup(self):
        self.unmount()
        if self.pending_image and not self.loop:
            # An interrupted losetup may have attached before its output was read.
            attached = self.command(['losetup', '--associated', self.pending_image,
                                     '--output', 'NAME', '--noheadings'], 'find-owned-loop').split()
            if len(attached) > 1:
                raise RuntimeError('ambiguous image attachment; manual cleanup required')
            self.loop = attached[0] if attached else None
        if self.loop:
            self.command(['losetup', '-d', self.loop], 'detach-loop')
            # losetup -d may be deferred; require attachment to have disappeared.
            backing = Path('/sys/class/block')/Path(self.loop).name/'loop/backing_file'
            if backing.exists():
                raise RuntimeError('loop detach incomplete')
            self.loop = None
        self.pending_image = None

    def work(self, mode, mount, kind, prefix):
        output = self.report / f'{prefix}-{mode}-{kind}.json'
        self.command([sys.executable, Path(__file__).resolve(), '_worker', mode, mount, kind,
                      self.report/'parameters.json', output], mode+'-'+kind, user=True, timeout=self.args.timeout)
        return json.loads(output.read_text())


def preflight(args):
    missing = []
    if sys.platform != 'linux' or os.geteuid() != 0:
        missing.append('Linux root required (invoke with sudo)')
    if int(os.environ.get('SUDO_UID', '0')) == 0 or int(os.environ.get('SUDO_GID', '0')) == 0:
        missing.append('non-root invoking SUDO_UID/SUDO_GID required')
    if Path('/proc/self/status').exists():
        caps = re.search(r'^CapEff:\s*([0-9a-f]+)', Path('/proc/self/status').read_text(), re.M)
        if caps and not int(caps[1], 16) & (1 << 21):
            missing.append('CAP_SYS_ADMIN required for mounting/loop management')
    tools_needed = {'lsblk', 'mount', 'umount'}
    tools_needed |= {'losetup'} if args.action == 'image' else {'wipefs', 'blockdev', 'udevadm'}
    for fs in args.filesystems:
        if fs.startswith('xffs'):
            tools_needed.add('fusermount3')
            if not Path('/dev/fuse').exists():
                missing.append('/dev/fuse unavailable')
            bindir = args.baseline_bin_dir if fs == 'xffs-0.0.1' else args.bin_dir
            for name in ('mkfs-xffs', 'mount-xffs'):
                if bindir is None or not os.access(bindir/name, os.X_OK):
                    missing.append(f'{fs}: executable {name} unavailable in {bindir}')
        else:
            tools_needed.add(FORMATS[fs])
            supported = {line.split()[-1] for line in Path('/proc/filesystems').read_text().splitlines()}
            if KERNEL[fs] not in supported:
                # Only inspect module availability; mount can auto-load it later.
                modprobe = shutil.which('modprobe')
                if not modprobe or subprocess.run([modprobe, '-n', KERNEL[fs]], capture_output=True, timeout=15).returncode:
                    missing.append(f'kernel filesystem support unavailable: {KERNEL[fs]}')
    for tool in sorted(tools_needed):
        if not shutil.which(tool):
            missing.append(f'missing tool: {tool}')
    if args.action == 'image' and not Path('/dev/loop-control').exists():
        missing.append('/dev/loop-control unavailable')
    if missing:
        raise Prerequisite('; '.join(dict.fromkeys(missing)))
    return sorted(tools_needed)


def provenance(runner, names):
    result = dict(kernel=list(os.uname()), python=sys.version,
                  harness_sha256=digest(Path(__file__)), tools={})
    paths = {name: Path(shutil.which(name)) for name in names}
    for fs in runner.args.filesystems:
        if fs.startswith('xffs'):
            folder = runner.args.baseline_bin_dir if fs == 'xffs-0.0.1' else runner.args.bin_dir
            for name in ('mkfs-xffs', 'mount-xffs'):
                paths[fs+'/'+name] = folder/name
    for name, path in paths.items():
        xffs = name.startswith('xffs')
        info = dict(path=str(path.resolve()), sha256=digest(path))
        version_flag = {'mkfs.exfat': '-V', 'mkfs.f2fs': '-V', 'mkfs.ext4': '-V', 'mkfs.fat': '--help'}.get(name, '--version')
        info['version_or_help'] = runner.command([path, '--help' if xffs else version_flag], 'tool-info', required=False, timeout=15)
        if name in ('mkfs.exfat', 'mkfs.fat', 'mkfs.f2fs'):
            info['help'] = runner.command([path, '-h' if name == 'mkfs.f2fs' else '--help'], 'tool-help', required=False, timeout=15)
            flags = {'mkfs.exfat': ['--no-discard', '--partition-table'], 'mkfs.fat': ['--mbr'], 'mkfs.f2fs': ['nodiscard']}[name]
            if any(flag not in info['help'] for flag in flags):
                raise Prerequisite(f'{name} lacks required formatting controls: {flags}')
        if xffs:
            required = ['--size-mib'] if name.endswith('mkfs-xffs') and runner.args.action == 'image' else ['--device']
            if name.endswith('mount-xffs'):
                required += ['--uid', '--gid', '--expect-disk-sequence', '--rw']
            elif runner.args.action == 'device':
                required += ['--erase', '--expect-serial']
            if any(flag not in info['version_or_help'] for flag in required):
                raise Prerequisite(f'{name} does not support required interface: {required}')
        if shutil.which('git'):
            info['git_head_near_binary'] = runner.command(['git', '-C', path.resolve().parent, 'rev-parse', 'HEAD'], 'git-info', required=False)
            info['git_status_near_binary'] = runner.command(['git', '-C', path.resolve().parent, 'status', '--porcelain'], 'git-info', required=False)
        result['tools'][name] = info
    return result


def report(data, output):
    if data.get('schema_version') != SCHEMA:
        raise ValueError('unsupported results schema')
    rows = []
    for sample in data['samples']:
        for kind in WORKLOADS:
            w = sample.get('workloads', {}).get(kind, {})
            good = (w.get('status') == 'measured' and w.get('verification') == 'verified'
                    and sample.get('status') == 'success' and sample.get('cleanup') == 'complete')
            rows.append(dict(filesystem=sample['filesystem'], repetition=sample['repetition'], workload=kind,
                             status='success' if good else ('unverified' if w.get('status') == 'measured' else w.get('status', 'missing')),
                             verification=w.get('verification', 'missing'), cleanup=sample.get('cleanup', 'missing'),
                             error=w.get('error', sample.get('error', '')),
                             **{k: w.get(k, '') if good else '' for k in ('seconds', 'bytes', 'operations', 'mib_s', 'ops_s', 'latency_median_s', 'latency_p95_s')}))
    with (output/'samples.csv').open('x', newline='') as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]) if rows else ['filesystem'])
        writer.writeheader()
        writer.writerows(rows)
    lines = ['# Filesystem comparison', '', data['measurement_scope'], '',
             f"Run status: **{data.get('status', 'incomplete')}**. Schema {SCHEMA}.", '',
             'Buffered application I/O, default durability settings. Bulk includes one final file fsync; durable includes fsync after every chunk. '
             'XFFS additionally synchronizes modifying filesystem operations internally, so equal application calls do not imply equal durability costs. '
             'Formatting, mounting, payload generation, hashing, verification and cleanup are excluded from timings. '
             'Fresh formatting does not reset flash-controller state. Image results include the host filesystem/cache.', '',
             'Only successful, remount-verified workloads with complete cleanup enter aggregates. p95 uses nearest rank. '
             'Latency scope is recorded in results.json; bulk write latency excludes its final fsync, and small-file latency excludes the final directory fsync.', '',
             '| Filesystem | Workload | Verified / planned | Seconds median [min, max] | MiB/s median [min, max] | Ops/s median [min, max] | Latency median seconds [min, max] | p95 seconds [min, max] |',
             '|---|---|---:|---|---|---|---|---|']
    for fs in data['filesystems']:
        for kind in WORKLOADS:
            selected = [r for r in rows if r['filesystem'] == fs and r['workload'] == kind and r['status'] == 'success']
            def summary(key):
                values = [r[key] for r in selected]
                return f'{statistics.median(values):.6g} [{min(values):.6g}, {max(values):.6g}]' if values else '—'
            lines.append(f"| {fs} ({'FUSE' if fs.startswith('xffs') else 'kernel'}) | {kind} | {len(selected)} / {data['repetitions']} | " + ' | '.join(summary(k) for k in ('seconds', 'mib_s', 'ops_s', 'latency_median_s', 'latency_p95_s'))+' |')
    lines += ['', '## Missing, failed, or unsupported samples', '']
    bad = [r for r in rows if r['status'] != 'success']
    lines += [f"- {r['filesystem']} repetition {r['repetition']} {r['workload']}: {r['status']}; verification={r['verification']}; cleanup={r['cleanup']}; {r['error']}" for r in bad] or ['None.']
    lines += ['', 'Exact commands, binary hashes and available Git provenance, device identity, mount options, capacities, parameters and cleanup are in results.json and command logs.', '']
    with (output/'comparison.md').open('x') as f:
        f.write('\n'.join(lines))


def execute(args):
    names = preflight(args)
    # Validate physical target and report storage before creating evidence.
    def inspect(argv):
        return subprocess.run(list(map(str, argv)), check=True, capture_output=True, text=True, timeout=15).stdout
    d = None
    if args.action == 'device':
        d = identity(args.device, inspect)
        validate_identity(d, args.expect_serial)
        refuse_busy(d, args.output)
    args.output.mkdir(parents=True, exist_ok=False)
    r = Runner(args, args.output)
    os.chown(args.output, r.uid, r.gid)
    r.device = d
    p = {k: getattr(args, k) for k in DEFAULTS}
    save(args.output/'parameters.json', p)
    matrix = [(n+1, fs) for n in range(args.repetitions) for fs in args.filesystems[n % len(args.filesystems):]+args.filesystems[:n % len(args.filesystems)]]
    data = dict(schema_version=SCHEMA, status='incomplete', measurement_scope='Host-backed image measurements' if args.action == 'image' else 'Whole removable USB disk measurements',
                parameters=p, timeout=args.timeout, image_mib=args.image_mib if args.action == 'image' else None,
                filesystems=args.filesystems, repetitions=args.repetitions, device=d,
                samples=[dict(filesystem=fs, repetition=n, status='missing', workloads={}, mounts=[], cleanup='not started') for n, fs in matrix])
    def persist():
        save(args.output/'results.json', data)
    persist()
    print('Run matrix (each entry receives a fresh format):', matrix, flush=True)
    if d:
        print('Authorized by --erase:', json.dumps(d), flush=True)
    try:
        data['environment'] = provenance(r, names)
        persist()
        for sample in data['samples']:
            print(f"Repetition {sample['repetition']}: {sample['filesystem']}", flush=True)
            sample['status'] = 'running'
            sample['cleanup'] = 'pending'
            persist()
            try:
                fs = sample['filesystem']
                r.format(fs, sample)
                persist()
                prefix = f"{sample['repetition']}-{fs}"
                with r.mount(fs, False, sample) as mount:
                    for kind in WORKLOADS:
                        sample['workloads'][kind] = dict(status='running')
                        persist()
                        try:
                            sample['workloads'][kind] = r.work('measure', mount, kind, prefix)
                        except BaseException as e:
                            sample['workloads'][kind] = dict(status='failed', error=str(e))
                            raise
                        persist()
                        if sample['workloads'][kind]['status'] != 'measured':
                            raise Unsupported(f'{kind}: synchronization unsupported')
                with r.mount(fs, True, sample) as mount:
                    for kind in WORKLOADS:
                        try:
                            result = r.work('verify', mount, kind, prefix)
                            if result['status'] != 'verified':
                                raise RuntimeError('verification did not succeed')
                            sample['workloads'][kind]['verification'] = 'verified'
                        except BaseException:
                            sample['workloads'][kind]['verification'] = 'failed'
                            raise
                        persist()
                sample['status'] = 'success'
            except BaseException as e:
                sample['status'], sample['error'] = 'failed', str(e)
                raise
            finally:
                try:
                    r.cleanup()
                    sample['cleanup'] = 'complete'
                except BaseException as e:
                    sample['cleanup'] = 'failed'
                    sample['cleanup_error'] = str(e)
                    sample['status'] = 'failed'
                    sample['error'] = sample.get('error', '') + f'; cleanup failed: {e}'
                    raise
                finally:
                    persist()
            # Retain images as evidence; no automatic destructive-run resume.
        data['status'] = 'complete'
        return 0
    except Prerequisite as e:
        data['error'] = str(e)
        print(f'UNMET PREREQUISITE: {e}', file=sys.stderr)
        return 77
    except BaseException as e:
        data['error'] = str(e) or type(e).__name__
        print(f'FAILED: {data["error"]}; evidence: {args.output}', file=sys.stderr)
        return 130 if isinstance(e, KeyboardInterrupt) else 1
    finally:
        persist()
        report(data, args.output)


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest='action', required=True)
    for action in ('image', 'device'):
        q = sub.add_parser(action)
        q.add_argument('--filesystems', nargs='+', choices=['xffs', 'exfat', 'fat32', 'ext4', 'f2fs', 'xffs-0.0.1'], default=['xffs', 'exfat', 'fat32', 'ext4'])
        q.add_argument('--bin-dir', type=Path, default=ROOT/'target/release')
        q.add_argument('--baseline-bin-dir', type=Path)
        q.add_argument('--repetitions', type=int, default=3)
        q.add_argument('--timeout', type=float, default=300)
        q.add_argument('--output', type=Path, default=Path('benchmark-'+time.strftime('%Y%m%d-%H%M%S')+'-'+uuid.uuid4().hex[:8]))
        q.add_argument('--image-mib', type=int, default=512)
        for name, value in DEFAULTS.items():
            q.add_argument('--'+name.replace('_', '-'), type=int, default=value)
        if action == 'device':
            q.add_argument('--device', type=Path, required=True)
            q.add_argument('--expect-serial', required=True)
            q.add_argument('--erase', action='store_true', required=True)
    q = sub.add_parser('report')
    q.add_argument('results', type=Path)
    q.add_argument('--output', type=Path, required=True, help='new report directory')
    return p


def main(argv=None):
    p = parser()
    a = p.parse_args(argv)
    a.output = a.output.resolve()
    if a.output.exists():
        p.error('output must be a new directory; previous runs are never overwritten')
    if a.action == 'report':
        data = json.loads(a.results.read_text())
        if data.get('schema_version') != SCHEMA:
            p.error('unsupported results schema')
        a.output.mkdir(parents=True)
        report(data, a.output)
        return 0
    if any(getattr(a, k) <= 0 for k in [*DEFAULTS, 'repetitions', 'image_mib', 'timeout']) or not math.isfinite(a.timeout):
        p.error('sizes, counts, repetitions and timeout must be positive and finite')
    if len(a.filesystems) != len(set(a.filesystems)):
        p.error('duplicate filesystem selection')
    if a.baseline_bin_dir and 'xffs-0.0.1' not in a.filesystems:
        a.filesystems.append('xffs-0.0.1')
    if 'fat32' in a.filesystems and max(getattr(a, k) for k in ('bulk_bytes', 'durable_bytes', 'small_bytes', 'replace_bytes')) > 2**32-1:
        p.error('FAT32 individual-file limit exceeded')
    a.bin_dir = a.bin_dir.resolve()
    if a.baseline_bin_dir:
        a.baseline_bin_dir = a.baseline_bin_dir.resolve()
    try:
        return execute(a)
    except Prerequisite as e:
        print(f'UNMET PREREQUISITE: {e}', file=sys.stderr)
        return 77
    except (OSError, RuntimeError, subprocess.SubprocessError) as e:
        print(f'FAILED: {e}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    def interrupted(*_):
        raise KeyboardInterrupt('terminated')
    signal.signal(signal.SIGTERM, interrupted)
    if len(sys.argv) > 1 and sys.argv[1] == '_worker':
        worker_main(sys.argv[2:])
    else:
        sys.exit(main())
