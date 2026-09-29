"""Host-only checks of acceptance evidence; never opens devices or formats disks."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

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

if __name__ == '__main__':
    unittest.main()
