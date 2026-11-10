from pathlib import Path
import subprocess
import sys
import unittest


class CaptureBackupGatewayFlagsTest(unittest.TestCase):
    def test_full_state_cannot_pass_without_undeleted_history(self):
        script = Path(__file__).with_name('test_capture_backup_gateway.py')
        result = subprocess.run([sys.executable, str(script), '--gateway-bin', '/missing-gateway-for-argument-validation', '--check-deletion', '--check-remote-replay', '--check-restored-state', '/missing-evidence-parent/result'], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('不能用空集合通過', result.stderr)

    def test_full_state_cannot_run_a_mode_that_exits_before_restore_checks(self):
        script = Path(__file__).with_name('test_capture_backup_gateway.py')
        result = subprocess.run([sys.executable, str(script), '--gateway-bin', '/missing-gateway-for-argument-validation', '--check-deletion', '--check-remote-witness', '--check-restored-state', '/missing-evidence-parent/result'], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('完整還原狀態驗收必須完成', result.stderr)

    def test_full_state_corruption_requires_full_state_validation(self):
        script = Path(__file__).with_name('test_capture_backup_gateway.py')
        result = subprocess.run([sys.executable, str(script), '--gateway-bin', '/missing-gateway-for-argument-validation', '--control-corrupt-restored-state'], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('還原內容反例必須搭配', result.stderr)

    def test_readonly_role_validation_cannot_exit_before_restored_data_checks(self):
        script = Path(__file__).with_name('test_capture_backup_gateway.py')
        for early_exit in ('--check-remote-witness', '--check-independent-position'):
            with self.subTest(flag=early_exit):
                result = subprocess.run([sys.executable, str(script), '--gateway-bin', '/missing-gateway-for-argument-validation', '--check-database-privileges', '--check-readonly-backup-role', '--check-deletion', early_exit], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('readonly backup validation requires --check-remote-replay', result.stderr)


if __name__ == '__main__':
    unittest.main()
