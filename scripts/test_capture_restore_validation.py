from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from scripts.capture_restore_validation import collect_gateway_log


class DiagnosticFailureTest(unittest.TestCase):
    def test_log_timeout_returns_error_so_cleanup_can_continue(self):
        with patch('scripts.capture_restore_validation.subprocess.run', side_effect=subprocess.TimeoutExpired('docker', 10)):
            error = collect_gateway_log(Path('/unused-evidence'), 'log', 'fixture')
        self.assertIn('TimeoutExpired', error)

    def test_evidence_write_failure_returns_error_so_cleanup_can_continue(self):
        with patch('scripts.capture_restore_validation.subprocess.run', return_value=subprocess.CompletedProcess([], 0, 'output', '')), patch('scripts.capture_restore_validation.write_evidence', side_effect=OSError('unwritable')):
            error = collect_gateway_log(Path('/unused-evidence'), 'log', 'fixture')
        self.assertIn('OSError', error)

    def test_missing_container_is_reported_as_failure(self):
        with tempfile.TemporaryDirectory() as directory, patch('scripts.capture_restore_validation.subprocess.run', return_value=subprocess.CompletedProcess([], 1, '', 'missing')):
            error = collect_gateway_log(Path(directory), 'log', 'fixture')
        self.assertEqual(error, 'Gateway取證命令失敗')


if __name__ == '__main__':
    unittest.main()
