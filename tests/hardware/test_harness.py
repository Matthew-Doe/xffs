"""Host-only checks of acceptance evidence; never opens devices or formats disks."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import contextlib
import io
import os
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('usb', ROOT / 'scripts/usb-acceptance.py')
usb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usb)

class EvidenceTests(unittest.TestCase):
    def setup_case(self, kind, phase):
        tmp = tempfile.TemporaryDirectory(prefix='xffs-evidence-test-')
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        mount, report = root / 'mount', root / 'report'
        mount.mkdir()
        report.mkdir()
        folder = mount / ('trial-' + kind)
        folder.mkdir()
        events = [{'event': 'intent', 'n': 0}]
        if phase:
            events.append({'event': 'ack', 'n': 0, 'phase': phase})
        (report / ('trial-' + kind + '.jsonl')).write_text('\n'.join(json.dumps(e) for e in events))
        return mount, report, folder / '000000'

    def test_acknowledged_immutable_data_must_survive_exactly(self):
        mount, report, path = self.setup_case('create', 'done')
        path.write_bytes(usb.payload('create', 0))
        usb.verify_trial(mount, report, 'create')
        path.write_bytes(b'corruption')
        with self.assertRaises(AssertionError):
            usb.verify_trial(mount, report, 'create')
        path.unlink()
        with self.assertRaises(AssertionError):
            usb.verify_trial(mount, report, 'create')

    def test_interrupted_rename_accepts_only_atomic_outcomes(self):
        mount, report, path = self.setup_case('replace', 'new')
        temp = path.with_suffix('.tmp')
        path.write_bytes(usb.payload('replace', 0, 'old'))
        temp.write_bytes(usb.payload('replace', 0))
        usb.verify_trial(mount, report, 'replace')
        temp.replace(path)
        usb.verify_trial(mount, report, 'replace')
        temp.write_bytes(usb.payload('replace', 0))
        with self.assertRaises(AssertionError):
            usb.verify_trial(mount, report, 'replace')

    def test_acknowledged_truncate_and_delete_are_enforced(self):
        mount, report, path = self.setup_case('cleanup', 'truncated')
        path.write_bytes(usb.payload('cleanup', 0))
        with self.assertRaises(AssertionError):
            usb.verify_trial(mount, report, 'cleanup')
        path.write_bytes(usb.payload('cleanup', 0)[:4096])
        usb.verify_trial(mount, report, 'cleanup')
        path.unlink()
        usb.verify_trial(mount, report, 'cleanup')

    def test_unknown_names_are_rejected(self):
        mount, report, path = self.setup_case('create', None)
        path.with_name('unrecorded').write_bytes(b'unknown')
        with self.assertRaises(AssertionError):
            usb.verify_trial(mount, report, 'create')

class CowTests(unittest.TestCase):
    def test_every_committed_prefix_and_preserved_partial_bytes(self):
        for n in [0, 1]:
            old, new, _, _ = usb.cow_versions(n)
            for cut in range(0, len(old) + 1, 4096):
                usb.verify_cow(new[:cut] + old[cut:], n, 'old', True)
            for cut in [200, 4095, 5000]:
                with self.assertRaises(AssertionError):
                    usb.verify_cow(new[:cut] + old[cut:], n, 'old', True)
            with self.assertRaises(AssertionError):
                usb.verify_cow(old[:4096] + new[4096:], n, 'old', True)
            for bad in [None, b'', new[:-1], old]:
                with self.assertRaises(AssertionError):
                    usb.verify_cow(bad, n, 'done', True)
            usb.verify_cow(new, n, 'done', True)
            usb.verify_cow(old, n, 'old', False)
            usb.verify_cow(None, n, None, False)
            usb.verify_cow(old[:100], n, None, False)
            for bad in [None, old[:-1], new + b'x']:
                with self.assertRaises(AssertionError):
                    usb.verify_cow(bad, n, 'old', True)
            with self.assertRaises(AssertionError):
                usb.verify_cow(new, n, 'old', False)

    def test_worker_and_verifier_use_existing_file_overwrites(self):
        with tempfile.TemporaryDirectory() as tmp:
            mount, report = Path(tmp) / 'mount', Path(tmp) / 'report'
            mount.mkdir()
            report.mkdir()
            usb.trial_worker(mount, report, 'cow', iterations=6)
            usb.verify_trial(mount, report, 'cow')
            self.assertTrue((report / 'ready-cow.json').exists())
            events = [json.loads(line) for line in (report / 'trial-cow.jsonl').read_text().splitlines()]
            self.assertEqual([e['event'] for e in events[:5]],
                             ['cow-start', 'initialize', 'initialized', 'initialize', 'initialized'])
            self.assertEqual([e['event'] for e in events[5:]],
                             ['cow-intent', 'cow-write-returned', 'cow-ack'] * 6)
            self.assertFalse(usb.cow_coverage_passed(report))
            path = mount / events[0]['folder'] / '000001'
            data = bytearray(path.read_bytes())
            data[-1] ^= 1
            path.write_bytes(data)
            with self.assertRaises(AssertionError):
                usb.verify_trial(mount, report, 'cow')

    def repeated_case(self, iterations=4):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        mount, report = root / 'mount', root / 'report'
        mount.mkdir()
        report.mkdir()
        usb.cow_worker(mount, report, iterations=iterations)
        events = [json.loads(x) for x in (report / 'trial-cow.jsonl').read_text().splitlines()]
        folder = mount / events[0]['folder']
        return mount, report, folder, events

    def test_pending_generation_and_interruption_coverage(self):
        mount, report, folder, events = self.repeated_case()
        # Two generations have completed on each file. Replace slot zero again.
        intent = {'event': 'cow-intent', 'n': 4, 'slot': 0, 'generation': 3}
        pending = events + [intent]
        old, new = usb.cow_state(0, 2), usb.cow_state(0, 3)
        path = folder / '000000'
        for cut in range(0, len(old) + 1, 4096):
            path.write_bytes(new[:cut] + old[cut:])
            result = usb.verify_cow_repeated(mount, pending)
            self.assertEqual(result['overwrite_interruption_observed'], cut in (4096, 8192))
            result = usb.verify_cow_repeated(mount, pending + [
                {'event': 'cow-interrupted', 'n': 4, 'stage': 'write'}])
            self.assertTrue(result['overwrite_interruption_observed'])
        path.write_bytes(new)
        result = usb.verify_cow_repeated(mount, pending + [
            {'event': 'cow-write-returned', 'n': 4},
            {'event': 'cow-interrupted', 'n': 4, 'stage': 'fsync'}])
        self.assertFalse(result['overwrite_interruption_observed'])
        for bad in [new[:100] + old[100:], old[:4096] + new[4096:], new[:-1]]:
            path.write_bytes(bad)
            with self.assertRaises(AssertionError):
                usb.verify_cow_repeated(mount, pending)
        path.unlink()
        with self.assertRaises(AssertionError):
            usb.verify_cow_repeated(mount, pending)

    def test_initialization_and_between_writes_cannot_pass_coverage(self):
        mount, report, folder, events = self.repeated_case()
        self.assertFalse(usb.verify_cow_repeated(mount, events)['overwrite_interruption_observed'])
        # An acknowledged generation cannot roll back.
        (folder / '000000').write_bytes(usb.cow_state(0, 1))
        with self.assertRaises(AssertionError):
            usb.verify_cow_repeated(mount, events)
        (folder / '000001').unlink()
        (folder / '000000').write_bytes(usb.cow_state(0, 0)[:100])
        result = usb.verify_cow_repeated(mount, events[:2])
        self.assertFalse(result['overwrite_interruption_observed'])

    def test_unaligned_pending_generation_preserves_untouched_bytes(self):
        mount, report, folder, events = self.repeated_case(iterations=5)
        events.append({'event': 'cow-intent', 'n': 5, 'slot': 1, 'generation': 3})
        old, new = usb.cow_state(1, 2), usb.cow_state(1, 3)
        path = folder / '000001'
        for cut in range(0, len(old) + 1, 4096):
            path.write_bytes(new[:cut] + old[cut:])
            usb.verify_cow_repeated(mount, events)
        damaged = bytearray(new)
        damaged[0] ^= 1
        path.write_bytes(damaged)
        with self.assertRaises(AssertionError):
            usb.verify_cow_repeated(mount, events)
        path.write_bytes(usb.cow_state(1, 1))
        with self.assertRaises(AssertionError):
            usb.verify_cow_repeated(mount, events)

    def test_completion_requires_coverage_after_recovery(self):
        for covered in [False, True]:
            with tempfile.TemporaryDirectory() as tmp:
                report = Path(tmp)
                usb.save(report / 'cow-coverage.json',
                         {'overwrite_interruption_observed': covered})
                h = mock.Mock(report=report)
                h.check.return_value = ''
                h.mount.return_value = contextlib.nullcontext(Path('/unused'))
                if covered:
                    usb.complete_trial(h, {'diskseq': 8}, 'cow')
                    self.assertTrue((report / 'passed-cow.json').exists())
                else:
                    with self.assertRaisesRegex(RuntimeError, 'no overwrite interruption'):
                        usb.complete_trial(h, {'diskseq': 8}, 'cow')
                    self.assertFalse((report / 'passed-cow.json').exists())
                    self.assertTrue((report / 'recovered-cow.json').exists())
                    self.assertFalse(json.loads((report / 'final.json').read_text())['acceptance_complete'])

    def test_disconnect_close_errors_are_recorded_but_other_errors_propagate(self):
        for expected, code in [(True, 5), (False, 5), (True, 13)]:
            with tempfile.TemporaryDirectory() as tmp:
                log = Path(tmp) / 'trial.jsonl'
                handles = [mock.Mock(), mock.Mock()]
                for handle in handles:
                    handle.close.side_effect = OSError(code, 'close failed')
                with mock.patch.object(Path, 'open', side_effect=handles):
                    # Mock record separately because Path.open is the file opener.
                    with mock.patch.object(usb, 'record') as record:
                        if expected and code == 5:
                            with usb.cow_handles(Path(tmp), log, [expected]):
                                pass
                            self.assertEqual(record.call_count, 2)
                        else:
                            with self.assertRaises(OSError):
                                with usb.cow_handles(Path(tmp), log, [expected]):
                                    pass
                for handle in handles:
                    handle.close.assert_called_once()

    def test_journal_phase_does_not_claim_torn_data_coverage(self):
        for first, second, phase in [
            ((11, False), (10, True), 'journal-retirement'),
            ((10, True), (10, True), 'committed-journal'),
            ((9, False), (10, True), 'journal-publication'),
            ((11, False), (11, False), 'clean-journal'),
        ]:
            text = '\n'.join(
                f'HOME control {n}: Ok(JournalControl {{ sequence: {seq}, committed: {str(state).lower()},'
                for n, (seq, state) in enumerate([first, second], 1))
            result = usb.cow_journal_observation(text)
            self.assertEqual(result['phase'], phase)
            self.assertFalse(result['data_block_write_interruption_proven'])
        self.assertEqual(usb.cow_journal_observation('corrupt')['phase'], 'unknown')

    def test_retry_preserves_evidence_and_excludes_old_removal(self):
        mount, report, folder, events = self.repeated_case()
        with self.assertRaisesRegex(RuntimeError, 'verify/recover'):
            usb.archive_cow_attempt(report)
        usb.save(report / 'recovered-cow.json', {'recovered': True})
        usb.record(report / 'events.jsonl', {'event': 'removal-observed', 'kind': 'cow',
                                          'device': {'serial': usb.SERIAL}})
        original = (report / 'trial-cow.jsonl').read_bytes()
        usb.archive_cow_attempt(report)
        archive = next(report.glob('cow-history-*'))
        self.assertEqual((archive / 'trial-cow.jsonl').read_bytes(), original)
        self.assertFalse((report / 'ready-cow.json').exists())
        self.assertTrue(folder.exists())
        usb.cow_worker(mount, report, iterations=1)
        self.assertEqual(len(list(mount.iterdir())), 2)
        with self.assertRaisesRegex(RuntimeError, 'no recorded removal'):
            usb.observed_removal(report, 'cow')



class ProcessTests(unittest.TestCase):
    def test_worker_output_is_visible(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / 'worker.log'
            with log.open('w') as stream:
                proc = subprocess.Popen([sys.executable, '-u', '-c', 'print("stage complete")'], stdout=stream, start_new_session=True)
            output = io.StringIO()
            try:
                with contextlib.redirect_stdout(output):
                    usb.wait_worker(proc, log, timeout=5)
                self.assertIn('stage complete', output.getvalue())
            finally:
                usb.stop_worker(proc)

    def test_stop_worker_allows_signal_cleanup(self):
        with tempfile.TemporaryDirectory() as tmp:
            ready, cleaned = Path(tmp) / 'ready', Path(tmp) / 'cleaned'
            code = ('import signal,time,pathlib,sys; '
                    'signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)); '
                    f'pathlib.Path({str(ready)!r}).touch(); '
                    '\ntry: time.sleep(30)\nfinally: '
                    f'pathlib.Path({str(cleaned)!r}).touch()')
            proc = subprocess.Popen([sys.executable, '-c', code], start_new_session=True)
            try:
                deadline = time.monotonic() + 5
                while not ready.exists():
                    self.assertIsNone(proc.poll())
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                usb.stop_worker(proc)
                self.assertTrue(cleaned.exists())
                self.assertEqual(proc.returncode, 0)
            finally:
                usb.stop_worker(proc)

    def test_mount_isolates_terminal_signals_and_preserves_interrupt(self):
        with tempfile.TemporaryDirectory() as tmp:
            h = usb.Harness.__new__(usb.Harness)
            h.report = Path(tmp)
            h.uid, h.gid = os.getuid(), os.getgid()
            h.user = {}
            h.command = mock.Mock(return_value=mock.Mock(returncode=1, stderr='LockContention'))
            proc = mock.Mock(returncode=0)
            with mock.patch.object(usb.subprocess, 'Popen', return_value=proc) as launch, \
                 mock.patch.object(usb, 'is_mounted', side_effect=[True, True, False, False]), \
                 mock.patch.object(usb, 'run') as unmount, \
                 mock.patch.object(usb.os, 'chown'), \
                 contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(KeyboardInterrupt):
                    with h.mount({'path': '/dev/test', 'diskseq': 1}, writable=True):
                        raise KeyboardInterrupt()
                self.assertTrue(launch.call_args.kwargs['start_new_session'])
                self.assertEqual(unmount.call_args.args[0][0:2], ['fusermount3', '-u'])
                proc.wait.assert_called_once_with(timeout=15)

    def test_retry_preserves_partial_workload_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            mount = Path(tmp)
            (mount / 'persist').mkdir()
            partial = mount / 'persist/throughput.bin'
            partial.write_bytes(b'partial evidence')
            with self.assertRaisesRegex(RuntimeError, 'use recover'):
                usb.workload(mount, mount)
            self.assertEqual(partial.read_bytes(), b'partial evidence')

class ClaimRetryTests(unittest.TestCase):
    def run_command(self, outcomes, times, **kwargs):
        with tempfile.TemporaryDirectory() as tmp:
            h = usb.Harness.__new__(usb.Harness)
            h.report = Path(tmp)
            h.events = h.report / 'events.jsonl'
            with mock.patch.object(usb.subprocess, 'run', side_effect=outcomes) as run, \
                 mock.patch.object(usb.time, 'monotonic', side_effect=times), \
                 mock.patch.object(usb.time, 'sleep'), \
                 contextlib.redirect_stdout(io.StringIO()):
                try:
                    h.command(['xffs-check', '--expect-serial', usb.SERIAL], 'check', **kwargs)
                except RuntimeError:
                    failed = True
                else:
                    failed = False
                events = [json.loads(line) for line in h.events.read_text().splitlines()]
                return run.call_count, failed, events, run.call_args_list

    def test_busy_then_success_retains_identity_arguments_and_evidence(self):
        busy = subprocess.CompletedProcess([], 1, '', 'Error: LockContention\n')
        ok = subprocess.CompletedProcess([], 0, 'valid', '')
        count, failed, events, calls = self.run_command([busy, ok], [0, 1, 2], retry_lock=True)
        self.assertEqual(count, 2)
        self.assertFalse(failed)
        self.assertEqual([e['exit'] for e in events], [1, 0])
        self.assertEqual(calls[0], calls[1])

    def test_persistent_busy_stops_at_deadline(self):
        busy = subprocess.CompletedProcess([], 1, '', 'Error: LockContention\n')
        count, failed, _, _ = self.run_command([busy, busy], [0, 1, 16], retry_lock=True)
        self.assertEqual(count, 2)
        self.assertTrue(failed)

    def test_other_errors_and_default_commands_are_never_retried(self):
        for error, retry in [('Error: IdentityChanged', True), ('Error: Corrupt', True),
                             ('Error: LockContention', False)]:
            result = subprocess.CompletedProcess([], 1, '', error)
            count, failed, _, _ = self.run_command([result], [0, 1], retry_lock=retry)
            self.assertEqual(count, 1)
            self.assertTrue(failed)

class ReconnectTests(unittest.TestCase):
    old = {'serial': usb.SERIAL, 'size': 124623257600, 'diskseq': 6}

    def test_early_enter_waits_for_absent_then_partial_then_new_device(self):
        new = dict(self.old, diskseq=7)
        with mock.patch.object(usb, 'identify', side_effect=[usb.DeviceNotReady(), FileNotFoundError(), new]), \
             mock.patch.object(usb.time, 'sleep'), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(usb.wait_for_reconnect(self.old), new)

    def test_old_sequence_is_not_accepted(self):
        new = dict(self.old, diskseq=7)
        with mock.patch.object(usb, 'identify', side_effect=[self.old, new]) as identify, \
             mock.patch.object(usb.time, 'sleep'), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(usb.wait_for_reconnect(self.old), new)
            self.assertEqual(identify.call_count, 2)

    def test_capacity_and_identity_failures_are_not_retried(self):
        for result in [dict(self.old, size=1, diskseq=7), RuntimeError('duplicate serial')]:
            with mock.patch.object(usb, 'identify', side_effect=[result]) as identify:
                with self.assertRaises(RuntimeError):
                    usb.wait_for_reconnect(self.old)
                self.assertEqual(identify.call_count, 1)

    def test_timeout_retains_pending_trial(self):
        with mock.patch.object(usb, 'identify', side_effect=usb.DeviceNotReady()), \
             mock.patch.object(usb.time, 'monotonic', side_effect=[0, 61]):
            with self.assertRaisesRegex(TimeoutError, 'resume-trial'):
                usb.wait_for_reconnect(self.old)

    def test_resume_requires_recorded_removal_and_unpassed_trial(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = Path(tmp)
            (report / 'trial-replace.jsonl').write_text('')
            (report / 'events.jsonl').write_text('')
            with self.assertRaisesRegex(RuntimeError, 'no recorded removal'):
                usb.observed_removal(report, 'replace')
            usb.record(report / 'events.jsonl', {'event': 'removal-observed', 'kind': 'replace', 'device': self.old})
            self.assertEqual(usb.observed_removal(report, 'replace'), self.old)
            (report / 'passed-replace.json').write_text('{}')
            with self.assertRaisesRegex(RuntimeError, 'already passed'):
                usb.observed_removal(report, 'replace')

class FinalizeTests(unittest.TestCase):
    def fixture(self, tmp):
        h = usb.Harness.__new__(usb.Harness)
        h.report = Path(tmp)
        h.command = mock.Mock()
        sysfs = h.report / 'sysfs'
        sysfs.mkdir()
        device = {'serial': usb.SERIAL, 'size': 124623257600, 'diskseq': 8,
                  'path': '/dev/test', 'sysfs': str(sysfs)}
        usb.save(h.report / 'format-started.json', {'device': device,
                 'uuid': '67b2fdfe-99c4-4062-98aa-d789adf9c572'})
        return h, device

    def test_receipt_requires_refresh_and_reverification_without_formatting(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, d = self.fixture(tmp)
            with mock.patch.object(usb, 'identify', return_value=d):
                usb.finalize_format(h, d)
            commands = [call.args[0] for call in h.command.call_args_list]
            self.assertEqual(commands[0], commands[-1])
            self.assertEqual(commands[1], ['blockdev', '--rereadpt', '/dev/test'])
            self.assertFalse(any('mkfs' in str(arg) for c in commands for arg in c))
            receipt = json.loads((h.report / 'formatted.json').read_text())
            self.assertTrue(receipt['passed'])
            self.assertFalse(receipt['original_format_command_succeeded'])

    def test_failed_verification_or_refresh_never_creates_receipt(self):
        for failure in [0, 1, 3]:
            with tempfile.TemporaryDirectory() as tmp:
                h, d = self.fixture(tmp)
                h.command.side_effect = [None] * failure + [RuntimeError('failed')]
                with mock.patch.object(usb, 'identify', return_value=d):
                    with self.assertRaises(RuntimeError):
                        usb.finalize_format(h, d)
                self.assertFalse((h.report / 'formatted.json').exists())

    def test_stale_kernel_partitions_prevent_receipt(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, d = self.fixture(tmp)
            child = Path(d['sysfs']) / 'child'
            child.mkdir()
            (child / 'partition').write_text('1')
            with mock.patch.object(usb, 'identify', return_value=d):
                with self.assertRaisesRegex(RuntimeError, 'still exposes partitions'):
                    usb.finalize_format(h, d)
            self.assertFalse((h.report / 'formatted.json').exists())

if __name__ == '__main__':
    unittest.main()
