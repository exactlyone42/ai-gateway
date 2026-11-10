import copy
from pathlib import Path
import runpy
import unittest


module = runpy.run_path(str(Path(__file__).with_name('client_slow_reader.py')))


class SlowReaderProofTest(unittest.TestCase):
    def sample(self):
        return {'samples': [{'phase': phase, 'seconds': offset + index * .25, 'source_bytes': count, 'rss': 20 * 1024 * 1024} for phase, offset, count in (('paused', 0, 1024 * 1024), ('paused_again', 5, 2 * 1024 * 1024)) for index in range(8)], 'baseline': {'rss': 16 * 1024 * 1024}, 'read_bytes': 128 * 1024, 'source_finished': False}

    def test_pressure_then_progress_is_required(self):
        proof = self.sample()
        module['validate_pressure'](proof)
        for row in proof['samples']:
            if row['phase'] == 'paused_again':
                row['source_bytes'] = 1024 * 1024
        with self.assertRaises(RuntimeError):
            module['validate_pressure'](proof)

    def test_fast_source_and_memory_growth_rejected(self):
        original = self.sample()
        proof = copy.deepcopy(original)
        proof['samples'][-1]['source_bytes'] += 64 * 1024
        with self.assertRaises(RuntimeError):
            module['validate_pressure'](proof)
        proof = copy.deepcopy(original)
        proof['samples'][-1]['rss'] = proof['baseline']['rss'] + module['RSS_LIMIT'] + 1
        with self.assertRaises(RuntimeError):
            module['validate_pressure'](proof)

    def test_unread_backlog_counts_consumed_bytes_separately(self):
        proof = self.sample()
        for row in proof['samples']:
            row['source_bytes'] += 10 * 1024 * 1024
            row['read_bytes'] = 10 * 1024 * 1024
        module['validate_pressure'](proof)
        proof['samples'][-1]['read_bytes'] = 0
        with self.assertRaises(RuntimeError):
            module['validate_pressure'](proof)

    def test_exhausted_source_and_short_window_rejected(self):
        for field in ('source_finished', 'samples'):
            proof = self.sample()
            proof[field] = True if field == 'source_finished' else proof['samples'][:-1]
            with self.assertRaises(RuntimeError):
                module['validate_pressure'](proof)

    def test_partial_prefix_never_allows_changed_or_extra_data(self):
        prefix = 'data: original\n\n'
        module['validate_prefix'](prefix + ': remaining\n', prefix)
        module['validate_prefix'](prefix, ': keepalive\n\n' + prefix)
        for wrong in ('data: changed\n\n', prefix + 'extra', ''):
            with self.assertRaises(RuntimeError):
                module['validate_prefix'](prefix, wrong)


if __name__ == '__main__':
    unittest.main()
