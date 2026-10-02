import contextlib
import io
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('batches', ROOT / 'scripts/profile-batches.py')
batches = importlib.util.module_from_spec(spec)
spec.loader.exec_module(batches)


class BatchProfileTests(unittest.TestCase):
    def test_rotated_order(self):
        order = batches.run_order()
        self.assertEqual(len(order), 12)
        self.assertEqual([k for r, k in order if r == 2], [16, 64, 256, 4])
        for kib in batches.LIMITS:
            self.assertEqual(sum(k == kib for _, k in order), 3)

    def test_medians_and_acceptance(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = Path(tmp)
            for repeat, kib in batches.run_order():
                run = report / f'rep-{repeat}-{kib}-kib'
                run.mkdir()
                def save(name, value):
                    (run / name).write_text(json.dumps(value))
                save('run.json', {'write_batch_kib': kib, 'mib_per_phase': 8,
                                 'profiling_enabled': True, 'mount_binary_sha256': 'same', 'scope': 'test'})
                save('cleanup.json', {'passed': True, 'free_space_restored': True})
                for phase in ('create', 'overwrite'):
                    save(phase + '-workload.json', {'verified': True, 'bytes': 8 * 1024 * 1024,
                                                  'write_fsync_seconds': (4 / kib) * repeat})
                    save(phase + '-profile.json', {'timings': {
                        'backend/flush': {'count': 14336 * 4 // kib, 'errors': 0},
                        'file_data/completed_batch': {'count': 2048 * 4 // kib,
                                                     'attempted_bytes': 8 * 1024 * 1024, 'errors': 0}}})
                save('summary.json', {phase: {
                    'workload': json.loads((run / (phase + '-workload.json')).read_text()),
                    'timings': json.loads((run / (phase + '-profile.json')).read_text())['timings']}
                    for phase in ('create', 'overwrite')})
            with contextlib.redirect_stdout(io.StringIO()):
                result = batches.summarize(report)
            self.assertTrue(result['performance_passed'])
            self.assertEqual(result['medians'][4]['create']['seconds'], 2)
            self.assertEqual(result['acceptance']['overwrite']['flush_reduction'], .9375)
            path = report / 'rep-1-4-kib/run.json'
            value = json.loads(path.read_text())
            value['mount_binary_sha256'] = 'different'
            path.write_text(json.dumps(value))
            with self.assertRaises(AssertionError):
                batches.summarize(report)
