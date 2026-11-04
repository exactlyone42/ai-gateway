import argparse
import json
import os
from pathlib import Path
import tempfile


def sync(source, destination, uid):
    try:
        with source.open('rb') as stream:
            raw = stream.read(65537)
        if len(raw) > 65536 or source.stat().st_mode & 0o077:
            raise ValueError('invalid credential file')
        auth = json.loads(raw)
        tokens = auth['tokens']
        if auth.get('auth_mode') != 'chatgpt':
            raise ValueError('subscription login required')
        selected = {'auth_mode': 'chatgpt', 'tokens': {
            'access_token': tokens['access_token'], 'account_id': tokens['account_id']}}
    except (OSError, ValueError, KeyError, TypeError):
        selected = {}
    content = json.dumps(selected).encode()
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if uid is not None:
        os.chown(destination.parent, uid, -1)
    try:
        if destination.read_bytes() == content:
            return
    except FileNotFoundError:
        pass
    descriptor, temporary = tempfile.mkstemp(dir=destination.parent)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
            if uid is not None:
                os.fchown(stream.fileno(), uid, -1)
        os.replace(temporary, destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description='將個人 Codex 存取憑證同步給 Gateway，不複製更新權杖')
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--uid', type=int)
    args = parser.parse_args()
    sync(args.source, args.destination, args.uid)
