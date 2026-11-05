from pathlib import Path
import subprocess
import sys
import unittest


class CaptureBackupGatewayFlagsTest(unittest.TestCase):
    def test_readonly_role_validation_cannot_exit_before_restored_data_checks(self):
        script = Path(__file__).with_name('test_capture_backup_gateway.py')
        for early_exit in ('--check-remote-witness', '--check-independent-position'):
            with self.subTest(flag=early_exit):
                result = subprocess.run([sys.executable, str(script), '--gateway-bin', '/missing-gateway-for-argument-validation', '--check-database-privileges', '--check-readonly-backup-role', '--check-deletion', early_exit], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('readonly backup validation requires --check-remote-replay', result.stderr)


if __name__ == '__main__':
    unittest.main()
