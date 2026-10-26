#!/usr/bin/env python3
import hashlib
import json
from pathlib import Path
import sys


def check(root):
    baseline = json.loads((root / 'applied-checksums.json').read_text())
    failures = []
    for name, checksum in baseline.items():
        target = root / name
        if not target.is_file() or hashlib.sha384(target.read_bytes()).hexdigest() != checksum:
            failures.append(name)
    if failures:
        raise RuntimeError('已發布的遷移檔被修改或移除：' + ', '.join(failures))
    print(f'已發布遷移檔 {len(baseline)} 個全部相符')


if __name__ == '__main__':
    try:
        check(Path(__file__).resolve().parent.parent / 'migrations')
    except (RuntimeError, OSError, ValueError) as error:
        sys.exit(str(error))
