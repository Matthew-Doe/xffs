#!/usr/bin/env python3
"""Compare durable-write batches in rotating order; device mode never formats."""
import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
LIMITS = (4, 16, 64, 256)


def run_order():
    return [(repeat + 1, kib) for repeat in range(3)
            for kib in LIMITS[repeat:] + LIMITS[:repeat]]


def summarize(report):
    samples = {kib: {'create': [], 'overwrite': []} for kib in LIMITS}
    hashes = set()
    scopes = set()
    devices = set()
    runs = []
    for repeat, kib in run_order():
        run = report / f'rep-{repeat}-{kib}-kib'
        provenance = json.loads((run / 'run.json').read_text())
        assert provenance['write_batch_kib'] == kib
        assert provenance['mib_per_phase'] == 8 and provenance['profiling_enabled']
        hashes.add(provenance['mount_binary_sha256'])
        devices.add(json.dumps(provenance.get('device'), sort_keys=True))
        scopes.add(provenance['scope'])
        cleanup = json.loads((run / 'cleanup.json').read_text())
        assert cleanup['passed'] and cleanup['free_space_restored']
        # Written only after all post-unmount checker calls have succeeded.
        completed = json.loads((run / 'summary.json').read_text())
        runs.append({'repeat': repeat, 'write_batch_kib': kib,
                     'provenance': provenance, 'cleanup': cleanup})
        for phase in ('create', 'overwrite'):
            work = json.loads((run / (phase + '-workload.json')).read_text())
            profile = json.loads((run / (phase + '-profile.json')).read_text())['timings']
            assert completed[phase] == {'workload': work, 'timings': profile}
            assert work['verified'] and work['bytes'] == 8 * 1024 * 1024
            assert all(metric['errors'] == 0 for metric in profile.values())
            assert profile['file_data/completed_batch']['attempted_bytes'] == work['bytes']
            samples[kib][phase].append({'repeat': repeat, 'workload': work, 'timings': profile})
    assert len(hashes) == len(scopes) == len(devices) == 1, 'binary or device identity changed'
    medians = {}
    for kib, phases in samples.items():
        medians[kib] = {}
        for phase, runs in phases.items():
            seconds = statistics.median(r['workload']['write_fsync_seconds'] for r in runs)
            medians[kib][phase] = {
                'seconds': seconds, 'mib_s': 8 / seconds,
                'flushes': statistics.median(r['timings']['backend/flush']['count'] for r in runs),
                'data_batches': statistics.median(r['timings']['file_data/completed_batch']['count'] for r in runs),
            }
    acceptance = {}
    for phase in ('create', 'overwrite'):
        baseline, default = medians[4][phase], medians[64][phase]
        reduction = 1 - default['flushes'] / baseline['flushes']
        acceptance[phase] = {
            'flush_reduction': reduction,
            'elapsed_speedup': baseline['seconds'] / default['seconds'],
            'passed': reduction >= .90 and default['seconds'] < baseline['seconds'],
        }
    result = {
        'scope': scopes.pop(), 'mount_binary_sha256': hashes.pop(),
        'default_kib': 64, 'medians': medians, 'acceptance': acceptance,
        'performance_passed': all(p['passed'] for p in acceptance.values()),
        'samples': samples, 'runs': runs,
    }
    (report / 'batch-summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: v for k, v in result.items() if k not in ('samples', 'runs')}, indent=2))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path, help='new host evidence directory')
    parser.add_argument('--device', action='store_true')
    parser.add_argument('--expect-serial', choices=['0085199340190280'])
    parser.add_argument('--bin-dir', type=Path, default=ROOT / 'target/release')
    parser.add_argument('--summarize-only', action='store_true', help='read completed runs without I/O to the filesystem under test')
    args = parser.parse_args()
    report = args.output.resolve()
    if not args.summarize_only:
        if args.device != bool(args.expect_serial):
            parser.error('--device and --expect-serial must be supplied together')
        report.mkdir(parents=True, exist_ok=False)
        for repeat, kib in run_order():
            command = [sys.executable, str(ROOT / 'scripts/profile-writes.py'),
                       '--output', str(report / f'rep-{repeat}-{kib}-kib'),
                       '--bin-dir', str(args.bin_dir.resolve()), '--size-mib', '8',
                       '--write-batch-kib', str(kib)]
            if args.device:
                command += ['--device', '--expect-serial', args.expect_serial]
            print(f'Repetition {repeat}/3, {kib} KiB', flush=True)
            subprocess.run(command, check=True, timeout=2400)
    result = summarize(report)
    if not result['performance_passed']:
        sys.exit('Performance criteria failed; inspect retained phase timings before accepting the milestone.')


if __name__ == '__main__':
    main()
