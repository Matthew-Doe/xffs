#!/usr/bin/env python3
"""Run explicitly requested disposable Linux device tests, prompting sudo locally.
Run as the ordinary user. This script never selects or formats a physical disk.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent

def run(args, **kwargs):
    return subprocess.run([str(x) for x in args], cwd=ROOT, check=True, timeout=240, **kwargs)

if os.geteuid() == 0:
    sys.exit('Run as your ordinary user; this script invokes sudo only for device tests.')
run(['cargo', 'build', '--workspace', '--locked'])
for package, test in [('xffs-core', 'linux_device'), ('xffs-tools', 'format_device')]:
    result = run(['cargo', 'test', '-p', package, '--test', test, '--no-run',
                  '--locked', '--message-format=json'], capture_output=True, text=True)
    print(result.stderr, end='')
    binaries = [entry['executable'] for line in result.stdout.splitlines()
                if (entry := json.loads(line)).get('reason') == 'compiler-artifact'
                and entry.get('executable') and entry['target']['name'] == test]
    assert len(binaries) == 1
    run(['sudo', binaries[0], '--ignored', '--nocapture', '--test-threads=1'])
run(['sudo', sys.executable, ROOT / 'scripts/device-mount-smoke.py'])
print('PASS: disposable backend, formatter, mounted-child refusal, and device FUSE tests')
