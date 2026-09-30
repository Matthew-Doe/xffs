#!/usr/bin/env python3
"""Explicit disposable-image integration check; never accepts physical devices.
Invoke through sudo as a non-root user. Requires prebuilt release tools.
Exit 77 means unmet prerequisites, not a passing integration test.
"""
import argparse
import json
from pathlib import Path
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', type=Path, default=ROOT/'target/release')
    parser.add_argument('--output', type=Path, required=True, help='new evidence directory')
    args = parser.parse_args()
    command = [sys.executable, ROOT/'scripts/benchmark-filesystems.py', 'image',
               '--bin-dir', args.bin_dir.resolve(), '--output', args.output.resolve(),
               '--repetitions', '1', '--bulk-bytes', '262144', '--bulk-chunk', '131072',
               '--durable-bytes', '8192', '--durable-chunk', '4096',
               '--small-count', '3', '--replace-count', '3', '--replace-bytes', '4096']
    # The runner owns subprocess deadlines and signal cleanup; do not kill it
    # with an outer timeout while it may be unmounting.
    proc = subprocess.Popen(list(map(str, command)), start_new_session=True)
    def interrupted(*_):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        code = proc.wait()
    except KeyboardInterrupt:
        # Forward once, then allow the runner's bounded teardown to finish.
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        proc.send_signal(signal.SIGTERM)
        return proc.wait()
    if code:
        return code
    data = json.loads((args.output/'results.json').read_text())
    assert data['status'] == 'complete'
    assert len(data['samples']) == 4
    for sample in data['samples']:
        assert sample['status'] == 'success' and sample['cleanup'] == 'complete'
        assert len(sample['mounts']) == 2
        assert 'ro' in sample['mounts'][1]['options'].split(',')
        assert all(w['status'] == 'measured' and w['verification'] == 'verified'
                   for w in sample['workloads'].values())
        assert len(sample['workloads']) == 4
    print('PASS: four-filesystem disposable-image matrix, remount verification and cleanup')
    return 0


if __name__ == '__main__':
    sys.exit(main())
