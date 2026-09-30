#!/usr/bin/env python3
"""Archive a clean tracked HEAD; untracked files are never included."""
import argparse
import gzip
import hashlib
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args], timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    if git('status', '--porcelain', '--untracked-files=no').strip():
        parser.error('tracked tree must be clean, including the index')
    commit = git('rev-parse', 'HEAD').decode().strip()
    name = 'xffs-0.0.1'
    out = args.output_dir.resolve()
    out.mkdir(parents=True, exist_ok=True)
    archive = out / (name + '.tar.gz')
    # git archive records HEAD in its tar pax header. Keep a readable sidecar too.
    with tempfile.TemporaryFile() as raw:
        subprocess.run(['git', '-C', str(ROOT), 'archive', '--format=tar',
                        '--prefix=' + name + '/', commit], stdout=raw, check=True, timeout=30)
        raw.seek(0)
        with archive.open('wb') as output, gzip.GzipFile(filename='', mode='wb', fileobj=output, mtime=0) as compressed:
            while chunk := raw.read(1024 * 1024):
                compressed.write(chunk)
    with archive.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    (out / (name + '.tar.gz.sha256')).write_text(f'{digest}  {archive.name}\n')
    (out / (name + '.source-commit')).write_text(commit + '\n')
    print(f'{archive}\nSource commit: {commit}\nSHA-256: {digest}')


if __name__ == '__main__':
    main()
