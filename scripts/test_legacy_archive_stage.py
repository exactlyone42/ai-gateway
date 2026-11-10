import io
import gzip
import os
from pathlib import Path
import resource
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from scripts import stage_legacy_backup as staging
from scripts.stage_legacy_backup import StageError, cleanup, stage


class LegacyArchiveStageTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dump = self.root / "source.dump"
        self.dump.write_bytes(b"PGDMP fixture")
        self.archive = self.root / "source.tgz"
        self.destination = self.root / "stage"
        self.destination.mkdir(mode=0o700)

    def archive_items(self, entries):
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, kind, content in entries:
                item = tarfile.TarInfo(name)
                item.type = kind
                item.size = len(content) if kind == tarfile.REGTYPE else 0
                item.linkname = "../../outside" if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE) else ""
                archive.addfile(item, io.BytesIO(content) if item.size else None)

    def test_valid_archive_is_rebuilt_without_changing_originals(self):
        name = "a" * 32 + ".ready"
        self.archive_items([("./", tarfile.DIRTYPE, b""), ("./" + name, tarfile.REGTYPE, b"fixture")])
        original = self.archive.read_bytes()
        stage(self.dump, self.archive, self.destination)
        self.assertEqual(self.archive.read_bytes(), original)
        self.assertEqual((self.destination / "gateway.dump").read_bytes(), b"PGDMP fixture")
        with tarfile.open(self.destination / "ledger-spool.tgz", "r:gz") as archive:
            self.assertEqual(archive.getnames(), [name])
            self.assertEqual(archive.extractfile(name).read(), b"fixture")
        self.assertEqual(os.stat(self.destination / "ledger-spool.tgz").st_mode & 0o777, 0o600)

    def test_rejects_links_traversal_nested_unknown_and_duplicates(self):
        name = "b" * 32 + ".ready"
        cases = [
            [("../escape", tarfile.REGTYPE, b"x")],
            [("/absolute", tarfile.REGTYPE, b"x")],
            [("folder/" + name, tarfile.REGTYPE, b"x")],
            [("./" + name, tarfile.SYMTYPE, b"")],
            [("./" + name, tarfile.LNKTYPE, b"")],
            [("./" + name, tarfile.FIFOTYPE, b"")],
            [("./unexpected", tarfile.REGTYPE, b"x")],
            [(name, tarfile.REGTYPE, b"x"), ("./" + name, tarfile.REGTYPE, b"y")],
        ]
        for index, entries in enumerate(cases):
            with self.subTest(index=index):
                self.archive_items(entries)
                dest = self.root / f"stage-{index}"
                dest.mkdir(mode=0o700)
                original = self.archive.read_bytes()
                with self.assertRaises(StageError):
                    stage(self.dump, self.archive, dest)
                self.assertEqual(self.archive.read_bytes(), original)

    def test_rejects_truncated_or_oversized_input_and_symlink(self):
        self.archive_items([("c" * 32 + ".ready", tarfile.REGTYPE, b"fixture")])
        original = self.archive.read_bytes()
        self.archive.write_bytes(original[: len(original) // 2])
        with self.assertRaises(StageError):
            stage(self.dump, self.archive, self.destination)
        symlink = self.root / "link.tgz"
        symlink.symlink_to(self.archive)
        dest = self.root / "stage-link"
        dest.mkdir(mode=0o700)
        with self.assertRaises((StageError, OSError)):
            stage(self.dump, symlink, dest)
        self.assertTrue(symlink.is_symlink())

    def test_cleanup_preserves_unexpected_files(self):
        unexpected = self.destination / "user-data"
        unexpected.write_bytes(b"preserved")
        with self.assertRaises(StageError):
            cleanup(self.destination)
        self.assertEqual(unexpected.read_bytes(), b"preserved")

    def test_staged_data_is_unchanged_when_originals_are_replaced(self):
        name = "e" * 32 + ".ready"
        self.archive_items([(name, tarfile.REGTYPE, b"original")])
        stage(self.dump, self.archive, self.destination)
        self.dump.write_bytes(b"replacement")
        self.archive.write_bytes(b"replacement")
        self.assertEqual((self.destination / "gateway.dump").read_bytes(), b"PGDMP fixture")
        with tarfile.open(self.destination / "ledger-spool.tgz", "r:gz") as archive:
            self.assertEqual(archive.extractfile(name).read(), b"original")

    def test_rejects_source_modified_during_copy(self):
        self.archive_items([("e" * 32 + ".ready", tarfile.REGTYPE, b"fixture")])
        check = staging.unchanged

        def modify_then_check(file, before):
            self.dump.write_bytes(b"changed-source")
            check(file, before)

        with patch.object(staging, "unchanged", side_effect=modify_then_check):
            with self.assertRaises(StageError):
                stage(self.dump, self.archive, self.destination)
        self.assertEqual(self.dump.read_bytes(), b"changed-source")

    def test_rejects_expansion_over_limit(self):
        self.archive_items([("f" * 32 + ".ready", tarfile.REGTYPE, b"x" * 4096)])
        with self.assertRaises(StageError):
            stage(self.dump, self.archive, self.destination, max_bytes=1024)

    def test_cli_rejects_large_extended_header_before_reading_payload(self):
        header = tarfile.TarInfo("PaxHeaders/large")
        header.type = tarfile.XHDTYPE
        header.size = 64 * 1024 * 1024
        with gzip.open(self.archive, "wb") as output:
            output.write(header.tobuf(format=tarfile.USTAR_FORMAT))
            block = bytes(1024 * 1024)
            for _ in range(64):
                output.write(block)
            output.write(bytes(1024))

        def limit_memory():
            resource.setrlimit(resource.RLIMIT_AS, (48 * 1024 * 1024, 48 * 1024 * 1024))

        script = Path(__file__).with_name("stage_legacy_backup.py")
        result = subprocess.run([sys.executable, str(script), "--dump", str(self.dump), "--archive", str(self.archive), "--destination", str(self.destination)], capture_output=True, text=True, timeout=30, preexec_fn=limit_memory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("備份內容不安全或無法讀取", result.stderr)
        self.assertEqual(self.dump.read_bytes(), b"PGDMP fixture")

    def test_rejects_invalid_or_missing_end_header_after_valid_member(self):
        name = "a" * 32 + ".ready"
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w:") as archive:
            item = tarfile.TarInfo(name)
            item.size = 1
            archive.addfile(item, io.BytesIO(b"x"))
        original = raw.getvalue()
        for label, payload in (("invalid", original[:1024] + b"X" * 512 + original[1536:]), ("missing", original[:1024])):
            with self.subTest(label=label):
                self.archive.write_bytes(gzip.compress(payload))
                dest = self.root / ("end-" + label)
                dest.mkdir(mode=0o700)
                with self.assertRaises(StageError):
                    stage(self.dump, self.archive, dest)

    def test_rejects_extended_header_without_reading_its_declared_payload(self):
        header = tarfile.TarInfo("PaxHeaders/large")
        header.type = tarfile.XHDTYPE
        header.size = 64 * 1024 * 1024
        self.archive.write_bytes(gzip.compress(header.tobuf(format=tarfile.USTAR_FORMAT)))
        with self.assertRaisesRegex(StageError, "入帳備份包含不預期的檔案"):
            stage(self.dump, self.archive, self.destination)

    def test_accepts_plain_gnu_and_ustar_formats(self):
        name = "9" * 32 + ".ready"
        for label, format_type in (("gnu", tarfile.GNU_FORMAT), ("ustar", tarfile.USTAR_FORMAT)):
            with self.subTest(label=label):
                with tarfile.open(self.archive, "w:gz", format=format_type) as archive:
                    item = tarfile.TarInfo(name)
                    item.size = 1
                    archive.addfile(item, io.BytesIO(b"x"))
                dest = self.root / ("format-" + label)
                dest.mkdir(mode=0o700)
                stage(self.dump, self.archive, dest)
                with tarfile.open(dest / "ledger-spool.tgz", "r:gz") as result:
                    self.assertEqual(result.extractfile(name).read(), b"x")

    def test_rejects_bad_header_checksum_crc_and_truncated_payload(self):
        item = tarfile.TarInfo("8" * 32 + ".ready")
        item.size = 1
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w:", format=tarfile.USTAR_FORMAT) as archive:
            archive.addfile(item, io.BytesIO(b"x"))
        original = raw.getvalue()
        altered_header = bytearray(original)
        altered_header[0] = ord("7")
        damaged_crc = bytearray(gzip.compress(original))
        damaged_crc[-8] ^= 1
        oversized_item = tarfile.TarInfo("8" * 32 + ".ready")
        oversized_item.size = 4096
        cases = (("checksum", gzip.compress(altered_header)), ("crc", damaged_crc), ("payload", gzip.compress(oversized_item.tobuf(format=tarfile.USTAR_FORMAT) + b"x")))
        for label, payload in cases:
            with self.subTest(label=label):
                self.archive.write_bytes(payload)
                dest = self.root / ("corrupt-" + label)
                dest.mkdir(mode=0o700)
                with self.assertRaises(StageError):
                    stage(self.dump, self.archive, dest)

    def test_cleanup_removes_only_generated_staging_files(self):
        self.archive_items([("d" * 32 + ".ready", tarfile.REGTYPE, b"fixture")])
        stage(self.dump, self.archive, self.destination)
        cleanup(self.destination)
        self.assertFalse(self.destination.exists())
        self.assertTrue(self.dump.exists())
        self.assertTrue(self.archive.exists())


if __name__ == "__main__":
    unittest.main()
