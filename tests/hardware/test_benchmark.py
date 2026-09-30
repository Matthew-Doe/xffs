"""Host-only benchmark tests. No test in this module opens a block device."""
import argparse
import contextlib
import csv
import errno
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('benchmark', ROOT/'scripts/benchmark-filesystems.py')
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)
SMALL = dict(bulk_bytes=128, bulk_chunk=32, durable_bytes=64, durable_chunk=16,
             small_count=3, small_bytes=20, replace_count=3, replace_bytes=24)


class Case(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)

    def runner(self):
        args = b.parser().parse_args(['image', '--filesystems', 'ext4', '--output', str(self.root)])
        return b.Runner(args, self.root)


class WorkerTests(Case):
    def test_all_workloads_verify_and_detect_corruption(self):
        for kind in b.WORKLOADS:
            with self.subTest(kind=kind):
                result = b.worker(self.root, kind, SMALL)
                self.assertEqual(result['status'], 'measured')
                self.assertGreater(result['seconds'], 0)
                self.assertGreater(result['mib_s'], 0)
                b.verify(self.root/kind, kind, SMALL)
                path = next((self.root/kind).iterdir())
                path.write_bytes(b'corrupted after remount')
                with self.assertRaisesRegex(RuntimeError, 'content mismatch'):
                    b.verify(self.root/kind, kind, SMALL)

    def test_namespace_verification(self):
        b.worker(self.root, 'replace', SMALL)
        (self.root/'replace'/'temporary').touch()
        with self.assertRaisesRegex(RuntimeError, 'namespace mismatch'):
            b.verify(self.root/'replace', 'replace', SMALL)

    def test_short_writes_and_zero_progress(self):
        values = []
        def write(fd, data):
            values.append(bytes(data))
            return min(2, len(data))
        with mock.patch.object(b.os, 'write', side_effect=write):
            b.write_all(9, b'abcdefg')
        self.assertEqual(values, [b'abcdefg', b'cdefg', b'efg', b'g'])
        with mock.patch.object(b.os, 'write', return_value=0):
            with self.assertRaises(OSError):
                b.write_all(9, b'abc')

    def test_fsync_order_profiles(self):
        for each, expected in [(False, ['open', 'write', 'write', 'sync', 'close']),
                               (True, ['open', 'write', 'sync', 'write', 'sync', 'close'])]:
            events = []
            def event(name, result=None):
                return lambda *a, **k: (events.append(name), result)[1]
            with mock.patch.object(b.os, 'open', side_effect=event('open', 9)), \
                 mock.patch.object(b, 'write_all', side_effect=event('write')), \
                 mock.patch.object(b, 'sync', side_effect=event('sync')), \
                 mock.patch.object(b.os, 'close', side_effect=event('close')):
                b.write_file(self.root/'file', [b'a', b'b'], each)
            self.assertEqual(events, expected)

    def test_directory_fsync_is_required(self):
        original = b.sync_dir
        calls = []
        def sync(path):
            calls.append(path)
            original(path)
        with mock.patch.object(b, 'sync_dir', side_effect=sync):
            b.worker(self.root, 'small', SMALL)
        self.assertEqual(calls, [self.root, self.root/'small'])
        calls.clear()
        with mock.patch.object(b, 'sync_dir', side_effect=sync):
            b.worker(self.root, 'replace', SMALL)
        self.assertEqual(calls, [self.root/'replace', self.root] + [self.root/'replace']*3)

    def test_unsupported_directory_fsync_is_not_success(self):
        with mock.patch.object(b.os, 'fsync', side_effect=OSError(errno.EINVAL, 'unsupported')):
            with self.assertRaises(b.Unsupported):
                b.sync_dir(self.root)
        config, output = self.root/'config.json', self.root/'result.json'
        config.write_text(json.dumps(SMALL))
        with mock.patch.object(b, 'sync_dir', side_effect=b.Unsupported('unsupported directory fsync')):
            b.worker_main(['measure', str(self.root), 'small', str(config), str(output)])
        self.assertEqual(json.loads(output.read_text())['status'], 'unsupported')


class SafetyTests(Case):
    def device(self):
        return dict(type='disk', tran='usb', rm=True, serial='expected', size=512*1024**2,
                    **{'log-sec': 512, 'diskseq': 5})

    def test_device_refusals_and_identity_changes(self):
        d = self.device()
        b.validate_identity(d, 'expected', d)
        for key, value in [('type', 'part'), ('tran', 'sata'), ('rm', False), ('serial', 'wrong'),
                           ('size', 0), ('log-sec', 123), ('diskseq', 6)]:
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                b.validate_identity(dict(d, **{key: value}), 'expected', d)

    def test_mounted_child_refused(self):
        with mock.patch.object(b, 'target_ids', return_value={'8:0', '8:1'}), \
             mock.patch.object(b, 'mounts', return_value=[dict(id='8:1')]):
            with self.assertRaisesRegex(RuntimeError, 'mounted'):
                b.refuse_busy({}, self.root)

    def test_swap_and_dependent_evidence_refused(self):
        for swaps, message in [('Filename Type Size Used Priority\n/tmp/file file 1 0 -2\n', 'swap'),
                               ('Filename Type Size Used Priority\n', 'evidence')]:
            with mock.patch.object(b, 'target_ids', return_value={'8:0'}), \
                 mock.patch.object(b, 'mounts', return_value=[]), \
                 mock.patch.object(Path, 'read_text', return_value=swaps), \
                 mock.patch.object(Path, 'stat', return_value=self.root.stat()), \
                 mock.patch.object(b, 'backing_ids', return_value={'8:0'}):
                with self.assertRaisesRegex(RuntimeError, message):
                    b.refuse_busy({}, self.root)

    def test_active_holders_refused(self):
        disk = self.root/'disk'
        (disk/'holders'/'dm-0').mkdir(parents=True)
        with self.assertRaisesRegex(RuntimeError, 'holders'):
            b.target_ids(dict(sysfs=str(disk)))

    def test_missing_prerequisites_exit_77(self):
        with mock.patch.object(b.shutil, 'which', return_value=None), \
             mock.patch.object(b.os, 'geteuid', return_value=1000), \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(b.main(['image', '--output', str(self.root/'new')]), 77)
        self.assertFalse((self.root/'new').exists())

    def test_explicit_missing_baseline_refused(self):
        args = b.parser().parse_args(['image', '--filesystems', 'xffs-0.0.1'])
        with self.assertRaisesRegex(b.Prerequisite, 'xffs-0.0.1'):
            b.preflight(args)

    def test_fat32_limit_and_no_overwrite(self):
        for options in [['--bulk-bytes', str(2**32), '--output', str(self.root/'new')],
                        ['--output', str(self.root)]]:
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as e:
                b.main(['image', *options])
            self.assertEqual(e.exception.code, 2)

    def test_mountinfo_readiness_does_not_stat_fuse(self):
        table = r'1 0 0:12 / /tmp/a\040b rw - fuse xffs rw,user_id=1000'+'\n'
        with mock.patch.object(Path, 'read_text', return_value=table), \
             mock.patch.object(Path, 'stat', side_effect=PermissionError):
            self.assertEqual(b.mounted('/tmp/a b')['filesystem'], 'fuse')
            self.assertIsNone(b.mounted('/tmp/a'))

    def test_format_controls(self):
        args = self.runner().args
        self.assertIn('nodiscard,lazy_itable_init=0,lazy_journal_init=0', b.format_args('ext4', '/unused', args))
        self.assertIn('-K', b.format_args('exfat', '/unused', args))
        self.assertIn('--mbr=n', b.format_args('fat32', '/unused', args))
        self.assertEqual(b.format_args('f2fs', '/unused', args)[1:4], ['-f', '-t', '0'])


class LifecycleTests(Case):
    def test_format_failure_is_not_retried(self):
        r = self.runner()
        with mock.patch.object(r, 'command', side_effect=RuntimeError('format failed')) as command:
            with self.assertRaisesRegex(RuntimeError, 'format failed'):
                r.format('ext4', dict(repetition=1))
        self.assertEqual(command.call_count, 1)
        self.assertEqual(command.call_args.args[1], 'format')

    def test_recheck_before_mount(self):
        r = self.runner()
        with mock.patch.object(r, 'recheck', side_effect=RuntimeError('identity changed')), \
             mock.patch.object(r, 'launch') as launch:
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                with r.mount('ext4', False, dict(mounts=[])):
                    self.fail('must not mount')
            launch.assert_not_called()

    def test_unready_mount_and_cleanup(self):
        r = self.runner()
        with mock.patch.object(r, 'recheck'), mock.patch.object(r, 'command'), \
             mock.patch.object(b, 'mounted', return_value=None), mock.patch.object(b.os, 'chown'):
            r.device = dict(path='/unused')
            with self.assertRaisesRegex(RuntimeError, 'mount not ready'):
                with r.mount('fat32', False, dict(mounts=[])):
                    self.fail('mount must fail')
            self.assertIsNone(r.mountpoint)

    def test_timeout_stops_group(self):
        r = self.runner()
        log = self.root/'log'
        log.touch()
        proc = mock.Mock()
        proc.poll.return_value = None
        with mock.patch.object(r, 'launch', return_value=(proc, log)), \
             mock.patch.object(b, 'stop') as stop:
            with self.assertRaises(TimeoutError):
                r.command(['unused'], timeout=0)
            stop.assert_called_once_with(proc)

    def test_interrupt_stops_group(self):
        r = self.runner()
        proc = mock.Mock()
        proc.poll.side_effect = KeyboardInterrupt
        with mock.patch.object(r, 'launch', return_value=(proc, self.root/'log')), \
             mock.patch.object(b, 'stop') as stop:
            with self.assertRaises(KeyboardInterrupt):
                r.command(['unused'])
            stop.assert_called_once_with(proc)

    def test_real_timeout_reaps_process(self):
        r = self.runner()
        with self.assertRaises(TimeoutError):
            r.command([sys.executable, '-c', 'import time; time.sleep(30)'], timeout=.1)
        self.assertTrue((self.root/'0001-command.log').exists())

    def test_signal_group_killed_even_when_leader_exits(self):
        proc = mock.Mock(pid=123)
        with mock.patch.object(b.os, 'killpg') as kill:
            b.stop(proc)
        self.assertEqual(kill.call_args_list, [mock.call(123, signal.SIGTERM), mock.call(123, signal.SIGKILL)])

    def test_teardown_failure_prevents_detach(self):
        r = self.runner()
        r.loop = '/unused-loop'
        with mock.patch.object(r, 'unmount', side_effect=RuntimeError('still mounted')), \
             mock.patch.object(r, 'command') as command:
            with self.assertRaises(RuntimeError):
                r.cleanup()
            command.assert_not_called()

    def test_unstopped_worker_prevents_unmount(self):
        r = self.runner()
        r.active_command = mock.Mock()
        with mock.patch.object(r, 'command') as command:
            with self.assertRaisesRegex(RuntimeError, 'active I/O'):
                r.cleanup()
            command.assert_not_called()

    def test_interrupted_attach_is_found_and_detached(self):
        r = self.runner()
        r.pending_image = self.root/'own.img'
        with mock.patch.object(r, 'command', side_effect=['/dev/test-loop\n', '']) as cmd, \
             mock.patch.object(Path, 'exists', return_value=False):
            r.cleanup()
        self.assertEqual(cmd.call_args_list[-1].args[0], ['losetup', '-d', '/dev/test-loop'])
        self.assertIsNone(r.loop)


class ReportTests(Case):
    def test_partial_report_regeneration_without_devices(self):
        data = dict(schema_version=1, measurement_scope='Host-backed image measurements', status='incomplete',
                    filesystems=['ext4'], repetitions=2, samples=[
                        dict(filesystem='ext4', repetition=1, status='success', cleanup='complete', workloads={
                            'bulk': dict(status='measured', verification='verified', seconds=1, bytes=1048576,
                                         operations=8, mib_s=1, ops_s=8, latency_median_s=.1, latency_p95_s=.2)}),
                        dict(filesystem='ext4', repetition=2, status='missing')])
        source = self.root/'results.json'
        source.write_text(json.dumps(data))
        output = self.root/'regenerated'
        with mock.patch.object(b, 'identity', side_effect=AssertionError('device access')), \
             mock.patch.object(b, 'preflight', side_effect=AssertionError('preflight')):
            self.assertEqual(b.main(['report', str(source), '--output', str(output)]), 0)
        with (output/'samples.csv').open() as f:
            rows = list(csv.DictReader(f))
        self.assertEqual(len(rows), 8)
        self.assertEqual(rows[0]['status'], 'success')
        self.assertEqual(rows[-1]['status'], 'missing')
        self.assertIn('1 / 2', (output/'comparison.md').read_text())
        self.assertIn('Host-backed', (output/'comparison.md').read_text())

    def test_failed_cleanup_excludes_measured_rates(self):
        data = dict(schema_version=1, measurement_scope='test', filesystems=['ext4'], repetitions=1,
                    samples=[dict(filesystem='ext4', repetition=1, status='success', cleanup='failed',
                                  workloads={'bulk': dict(status='measured', verification='verified', mib_s=9)})])
        b.report(data, self.root)
        with (self.root/'samples.csv').open() as f:
            self.assertEqual(next(csv.DictReader(f))['mib_s'], '')
        self.assertIn('0 / 1', (self.root/'comparison.md').read_text())


class MatrixTests(Case):
    def exercise(self, failure=None, repetitions=1):
        owner = self
        calls = []
        class FakeRunner(b.Runner):
            def format(self, fs, sample):
                calls.append(('format', fs, sample['repetition']))
                self.current = owner.root/f"mount-{fs}-{sample['repetition']}"
                self.current.mkdir()
                if failure == 'format':
                    raise RuntimeError('injected format failure')

            @contextlib.contextmanager
            def mount(self, fs, readonly, sample):
                sample['mounts'].append(dict(options='ro' if readonly else 'rw'))
                calls.append(('mount', readonly))
                yield self.current
                calls.append(('unmount', readonly))

            def work(self, mode, mount, kind, prefix):
                if failure == 'timeout' and mode == 'measure' and kind == 'durable':
                    raise TimeoutError('workload deadline exceeded')
                if failure == 'unsupported' and kind == 'small':
                    return dict(status='unsupported', error='directory fsync unsupported')
                if mode == 'verify':
                    if failure == 'corrupt' and kind == 'bulk':
                        (mount/kind/'data').write_bytes(b'corrupted remount')
                    b.verify(mount/kind, kind, SMALL)
                    return dict(status='verified')
                return b.worker(mount, kind, SMALL)

            def cleanup(self):
                calls.append(('cleanup',))
                if failure == 'cleanup':
                    raise RuntimeError('injected unmount failure')

        output = self.root/'report'
        argv = ['image', '--filesystems', 'ext4', 'fat32', '--repetitions', str(repetitions), '--output', str(output)]
        for key, value in SMALL.items():
            argv += ['--'+key.replace('_', '-'), str(value)]
        with mock.patch.object(b, 'Runner', FakeRunner), mock.patch.object(b, 'preflight', return_value=[]), \
             mock.patch.object(b, 'provenance', return_value={}), mock.patch.object(b.os, 'chown'), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = b.main(argv)
        return code, json.loads((output/'results.json').read_text()), calls

    def test_rotating_matrix_and_remount_success(self):
        code, data, calls = self.exercise(repetitions=2)
        self.assertEqual(code, 0)
        self.assertEqual(data['status'], 'complete')
        self.assertEqual([c for c in calls if c[0] == 'format'],
                         [('format', 'ext4', 1), ('format', 'fat32', 1),
                          ('format', 'fat32', 2), ('format', 'ext4', 2)])
        self.assertTrue(all(s['cleanup'] == 'complete' and s['status'] == 'success' for s in data['samples']))
        self.assertEqual(calls[1:6], [('mount', False), ('unmount', False), ('mount', True), ('unmount', True), ('cleanup',)])

    def test_remount_corruption_stops_matrix(self):
        code, data, calls = self.exercise('corrupt')
        self.assertEqual(code, 1)
        self.assertEqual(data['samples'][0]['workloads']['bulk']['verification'], 'failed')
        self.assertEqual(data['samples'][1]['status'], 'missing')
        self.assertEqual(sum(c[0] == 'format' for c in calls), 1)

    def test_timeout_has_no_rate_and_stops_matrix(self):
        code, data, calls = self.exercise('timeout')
        w = data['samples'][0]['workloads']['durable']
        self.assertEqual(code, 1)
        self.assertEqual(w['status'], 'failed')
        self.assertNotIn('mib_s', w)
        self.assertEqual(data['samples'][0]['cleanup'], 'complete')
        self.assertEqual(data['samples'][1]['status'], 'missing')

    def test_unsupported_is_incomplete(self):
        code, data, calls = self.exercise('unsupported')
        self.assertEqual(code, 1)
        self.assertEqual(data['status'], 'incomplete')
        self.assertEqual(data['samples'][0]['workloads']['small']['status'], 'unsupported')
        self.assertEqual(data['samples'][1]['status'], 'missing')

    def test_cleanup_failure_stops_matrix_and_excludes_aggregates(self):
        code, data, calls = self.exercise('cleanup')
        self.assertEqual(code, 1)
        self.assertEqual(data['samples'][0]['cleanup'], 'failed')
        self.assertEqual(data['samples'][1]['status'], 'missing')
        with (self.root/'report/samples.csv').open() as f:
            self.assertTrue(all(r['mib_s'] == '' for r in csv.DictReader(f)))

    def test_format_failure_does_not_advance(self):
        code, data, calls = self.exercise('format')
        self.assertEqual(code, 1)
        self.assertEqual(calls, [('format', 'ext4', 1), ('cleanup',)])
        self.assertEqual(data['samples'][1]['status'], 'missing')


if __name__ == '__main__':
    unittest.main()
