import argparse
import gzip
import os
from pathlib import Path, PurePosixPath
import re
import resource
import stat
import sys
import tarfile
import zlib


MAX_BYTES = 16 * 1024 * 1024 * 1024
MAX_ENTRIES = 100000
MAX_MEMORY = 128 * 1024 * 1024
NAMES = re.compile(r"(?:[0-9a-f]{32}\.(?:ready|reserved|working|next|conflict)|\.ledger-spool\.lock|\.capture-backup\.lock)")


class StageError(Exception):
    pass


class BoundedReader:
    def __init__(self, file, limit):
        self.file = file
        self.limit = limit
        self.total = 0

    def read(self, size=-1):
        if size < 0:
            size = 1024 * 1024
        data = self.file.read(min(size, self.limit - self.total + 1))
        self.total += len(data)
        if self.total > self.limit:
            raise StageError("備份展開後超過上限")
        return data


class PayloadReader:
    def __init__(self, file, size):
        self.file = file
        self.remaining = size

    def read(self, size=-1):
        if size < 0:
            size = self.remaining
        size = min(size, self.remaining)
        data = self.file.read(size)
        if len(data) != size:
            raise StageError("入帳備份內容不完整")
        self.remaining -= size
        return data


def read_exact(file, size):
    data = file.read(size)
    if len(data) != size:
        raise StageError("入帳備份內容不完整")
    return data


def open_source(path, max_bytes):
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except OSError as error:
        raise StageError("無法讀取備份") from error
    info = os.fstat(descriptor)
    if not stat.S_ISREG(info.st_mode) or info.st_size <= 0 or info.st_size > max_bytes:
        os.close(descriptor)
        raise StageError("備份檔案類型或大小不正確")
    return os.fdopen(descriptor, "rb"), info


def unchanged(file, before):
    after = os.fstat(file.fileno())
    fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, field) != getattr(after, field) for field in fields):
        raise StageError("備份讀取期間已變更")


def create_staged(directory, name):
    descriptor = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=directory)
    return os.fdopen(descriptor, "wb")


def open_stage(destination):
    descriptor = os.open(destination, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    info = os.fstat(descriptor)
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        os.close(descriptor)
        raise StageError("暫存目錄不安全")
    return descriptor


def stage(dump, archive, destination, max_bytes=MAX_BYTES):
    directory = open_stage(destination)
    try:
        source, before = open_source(dump, max_bytes)
        with source, create_staged(directory, "gateway.dump") as output:
            total = 0
            for block in iter(lambda: source.read(1024 * 1024), b""):
                total += len(block)
                if total > max_bytes:
                    raise StageError("資料庫備份超過上限")
                output.write(block)
            unchanged(source, before)
            output.flush()
            os.fsync(output.fileno())
        source, before = open_source(archive, max_bytes)
        with source, create_staged(directory, "ledger-spool.tgz") as output:
            with gzip.GzipFile(fileobj=source, mode="rb") as compressed:
                bounded = BoundedReader(compressed, max_bytes)
                with tarfile.open(fileobj=output, mode="w|gz") as outgoing:
                    seen = set()
                    while True:
                        header = read_exact(bounded, tarfile.BLOCKSIZE)
                        if header == bytes(tarfile.BLOCKSIZE):
                            if read_exact(bounded, tarfile.BLOCKSIZE) != bytes(tarfile.BLOCKSIZE):
                                raise StageError("入帳備份結尾不正確")
                            while tail := bounded.read(1024 * 1024):
                                if any(tail):
                                    raise StageError("入帳備份結尾包含額外資料")
                            break
                        item = tarfile.TarInfo.frombuf(header, encoding="utf-8", errors="strict")
                        path = PurePosixPath(item.name)
                        name = str(path)
                        if path.is_absolute() or ".." in path.parts or "\x00" in item.name or name in seen:
                            raise StageError("入帳備份路徑不安全或重複")
                        seen.add(name)
                        if len(seen) > MAX_ENTRIES:
                            raise StageError("入帳備份檔案過多")
                        if item.isdir() and name == "." and item.size == 0:
                            continue
                        if len(path.parts) != 1 or not NAMES.fullmatch(name) or item.type not in (tarfile.REGTYPE, tarfile.AREGTYPE):
                            raise StageError("入帳備份包含不預期的檔案")
                        if item.size < 0 or item.size > max_bytes or (name.endswith(".lock") and item.size != 0):
                            raise StageError("入帳備份檔案大小不正確")
                        data = PayloadReader(bounded, item.size)
                        clean = tarfile.TarInfo(name)
                        clean.size = item.size
                        clean.mode = 0o600
                        outgoing.addfile(clean, data)
                        if data.remaining:
                            raise StageError("入帳備份內容不完整")
                        padding = (-item.size) % tarfile.BLOCKSIZE
                        if padding:
                            read_exact(bounded, padding)
            unchanged(source, before)
            output.flush()
            os.fsync(output.fileno())
        os.fsync(directory)
    except (OSError, EOFError, tarfile.TarError, zlib.error, ValueError) as error:
        raise StageError("備份無法安全檢查") from error
    finally:
        os.close(directory)


def cleanup(destination):
    directory = open_stage(destination)
    try:
        names = os.listdir(directory)
        if any(name not in {"gateway.dump", "ledger-spool.tgz"} for name in names):
            raise StageError("暫存目錄含有非預期資料")
        for name in names:
            if not stat.S_ISREG(os.stat(name, dir_fd=directory, follow_symlinks=False).st_mode):
                raise StageError("暫存檔案類型不正確")
        current = os.stat(destination, follow_symlinks=False)
        opened = os.fstat(directory)
        if (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino):
            raise StageError("暫存目錄已變更")
        for name in names:
            os.unlink(name, dir_fd=directory)
        os.rmdir(destination)
    finally:
        os.close(directory)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dump", type=Path)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--destination", type=Path)
    parser.add_argument("--cleanup", type=Path)
    args = parser.parse_args()
    try:
        soft, hard = resource.getrlimit(resource.RLIMIT_AS)
        limit = MAX_MEMORY if soft == resource.RLIM_INFINITY else min(soft, MAX_MEMORY)
        resource.setrlimit(resource.RLIMIT_AS, (limit, hard))
        if args.cleanup and not any((args.dump, args.archive, args.destination)):
            cleanup(args.cleanup)
        elif not args.cleanup and all((args.dump, args.archive, args.destination)):
            stage(args.dump, args.archive, args.destination)
        else:
            raise StageError("暫存參數不正確")
    except (StageError, OSError, ValueError, MemoryError):
        print("備份內容不安全或無法讀取，未開始還原。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
