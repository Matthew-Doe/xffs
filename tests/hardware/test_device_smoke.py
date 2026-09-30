"""Mount readiness must work even when root cannot stat a user's FUSE mount."""
import importlib.util
from pathlib import Path
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('device_smoke', ROOT / 'scripts/device-mount-smoke.py')
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class MountDetectionTests(unittest.TestCase):
    def test_inaccessible_fuse_mount_is_visible_for_readiness_and_cleanup(self):
        table = '123 1 0:72 / /tmp/test/mount ro - fuse xffs ro,user_id=1000\n'
        with mock.patch.object(Path, 'read_text', return_value=table), \
             mock.patch.object(Path, 'stat', side_effect=PermissionError('FUSE owner only')):
            self.assertTrue(smoke.mounted('/tmp/test/mount'))
            self.assertFalse(smoke.mounted('/tmp/test'))
            self.assertFalse(smoke.mounted('/tmp/test/mount-other'))
        with mock.patch.object(Path, 'read_text', return_value=''):
            self.assertFalse(smoke.mounted('/tmp/test/mount'))

    def test_mountinfo_escaped_paths(self):
        table = r'123 1 0:72 / /tmp/a\040b\134c\011d\012e ro - fuse xffs ro' + '\n'
        with mock.patch.object(Path, 'read_text', return_value=table):
            self.assertTrue(smoke.mounted('/tmp/a b\\c\td\ne'))


class CheckRetryTests(unittest.TestCase):
    def exercise(self, results):
        import subprocess
        now = [0.0]
        def sleep(delay):
            now[0] += delay
        with mock.patch.object(smoke.subprocess, 'run', side_effect=results) as run, \
             mock.patch.object(smoke.time, 'monotonic', side_effect=lambda: now[0]), \
             mock.patch.object(smoke.time, 'sleep', side_effect=sleep), \
             mock.patch('sys.stdout'), mock.patch('sys.stderr'):
            try:
                smoke.check_device('/dev/loop-test', 42)
            except subprocess.CalledProcessError:
                failed = True
            else:
                failed = False
        for call in run.call_args_list:
            self.assertEqual(call.args[0][-2:], ['--expect-disk-sequence', '42'])
        return run.call_count, now[0], failed

    def result(self, error=''):
        import subprocess
        return subprocess.CompletedProcess([], int(bool(error)), '', error)

    def test_transient_busy_then_success(self):
        self.assertEqual(self.exercise([self.result('Error: LockContention'), self.result()]),
                         (2, .1, False))

    def test_busy_deadline(self):
        calls, elapsed, failed = self.exercise([self.result('Error: LockContention')] * 200)
        self.assertGreater(calls, 100)
        self.assertEqual(elapsed, 15)
        self.assertTrue(failed)

    def test_permanent_or_later_errors_fail_immediately(self):
        for error in ['Error: IdentityChanged', 'Error: UnsafeTopology',
                      'read failed: LockContention']:
            self.assertEqual(self.exercise([self.result(error)]), (1, 0, True))
