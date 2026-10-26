import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch


SPEC = importlib.util.spec_from_file_location(
    'deploy_gateway', Path(__file__).with_name('deploy_gateway.py')
)
deploy_gateway = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(deploy_gateway)


class MigrationTests(unittest.TestCase):
    def test_exact_match_is_accepted(self):
        deploy_gateway.verify_migrations({'1': 'abc'}, '1|abc|t\n')

    def test_changed_missing_extra_failed_and_empty_are_rejected(self):
        for applied in ['1|def|t', '', '1|abc|t\n2|def|t', '1|abc|f']:
            with self.subTest(applied=applied), self.assertRaises(RuntimeError):
                deploy_gateway.verify_migrations({'1': 'abc'}, applied)
        with self.assertRaises(RuntimeError):
            deploy_gateway.verify_migrations({'1': 'abc', '2': 'def'}, '1|abc|t')


class RouteTests(unittest.TestCase):
    def test_port_prefix_is_not_replaced(self):
        with self.assertRaises(RuntimeError):
            deploy_gateway.replace_route('reverse_proxy 127.0.0.1:180800', '18080', 18081)
        self.assertEqual(
            deploy_gateway.replace_route('reverse_proxy 127.0.0.1:18080\n', '18080', 18081),
            'reverse_proxy 127.0.0.1:18081\n',
        )


class ReloadTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.config = Path(self.temp.name) / 'Caddyfile'
        self.config.write_text('old')

    def test_invalid_config_does_not_write_or_reload(self):
        with patch.object(deploy_gateway, 'run', side_effect=RuntimeError('invalid')) as run:
            with self.assertRaises(RuntimeError):
                deploy_gateway.reload_caddy('caddy', self.config, 'old', 'new')
        self.assertEqual(self.config.read_text(), 'old')
        self.assertEqual(run.call_count, 1)
        self.assertIn('validate', run.call_args.args[0])

    def test_reload_failure_restores_previous_before_second_reload(self):
        seen = []

        def run(command, **kwargs):
            seen.append((command, self.config.read_text()))
            if len(seen) == 2:
                raise RuntimeError('reload failed')
            return b''

        with patch.object(deploy_gateway, 'run', side_effect=run):
            with self.assertRaises(RuntimeError):
                deploy_gateway.reload_caddy('caddy', self.config, 'old', 'new')
        self.assertEqual([value for _, value in seen], ['old', 'new', 'old'])
        self.assertIn('validate', seen[0][0])
        self.assertIn('reload', seen[1][0])
        self.assertIn('reload', seen[2][0])
        self.assertEqual(self.config.read_text(), 'old')

    def test_concurrent_edit_is_preserved(self):
        self.config.write_text('other')
        with patch.object(deploy_gateway, 'run') as run:
            with self.assertRaises(RuntimeError):
                deploy_gateway.reload_caddy('caddy', self.config, 'old', 'new')
        run.assert_not_called()
        self.assertEqual(self.config.read_text(), 'other')


class StreamCompletionTests(unittest.TestCase):
    def test_content_must_continue_after_switch_offset(self):
        with tempfile.TemporaryDirectory() as folder:
            evidence = Path(folder)
            output = evidence / 'cutover-stream.jsonl'
            delta = json.dumps({'type': 'stream_event', 'event': {'type': 'content_block_delta'}}).encode() + b'\n'
            result = json.dumps({'type': 'result', 'is_error': False, 'result': 'GATEWAY_STREAM_END'}).encode() + b'\n'
            process = Mock()
            process.wait.return_value = 0
            output.write_bytes(delta + result)
            with self.assertRaisesRegex(RuntimeError, '切換後沒有新的串流內容'):
                deploy_gateway.finish_stream_probe(process, evidence, len(delta))
            output.write_bytes(delta + delta + result)
            deploy_gateway.finish_stream_probe(process, evidence, len(delta))


class RetirementTests(unittest.TestCase):
    def test_active_or_unknown_count_keeps_old_service(self):
        for health in [{'active_requests': 1}, {}]:
            with self.subTest(health=health):
                with patch.object(deploy_gateway, 'fetch', return_value=health), patch.object(deploy_gateway, 'run') as run:
                    self.assertFalse(deploy_gateway.retire_idle_source('old', '18080'))
                    run.assert_not_called()

    def test_existing_tcp_connection_keeps_old_service(self):
        connection = b'0: 00000000:1F90 0100007F:1234 01 00000000:00000000\n'
        with patch.object(deploy_gateway, 'fetch', return_value={'active_requests': 0}), patch.object(deploy_gateway, 'run', return_value=connection) as run:
            self.assertFalse(deploy_gateway.retire_idle_source('old', '18080'))
            self.assertEqual(run.call_count, 1)
            self.assertNotIn('stop', run.call_args.args[0])

    def test_three_idle_checks_precede_stop(self):
        events = []

        def fetch(*args):
            events.append('health')
            return {'active_requests': 0}

        def run(command):
            events.append('stop' if 'stop' in command else 'tcp')
            return b''

        with patch.object(deploy_gateway, 'fetch', side_effect=fetch), patch.object(deploy_gateway, 'run', side_effect=run), patch.object(deploy_gateway.time, 'sleep'):
            self.assertTrue(deploy_gateway.retire_idle_source('old', '18080'))
        self.assertEqual(events, ['health', 'tcp'] * 3 + ['stop'])


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.config = root / 'Caddyfile'
        self.previous = 'gateway.test {\n reverse_proxy 127.0.0.1:18080\n}\n'
        self.config.write_text(self.previous)
        self.args = argparse.Namespace(
            evidence=str(root / 'evidence'), source_container='old', image='new-image',
            port=18081, caddyfile=str(self.config), database_container='database',
            revision='123456abcdef', caddy_container='caddy', public_url='https://gateway.test',
        )
        self.events = []
        self.failure = None
        self.stream_process = Mock()
        self.stream_process.poll.return_value = None
        for name in ['run', 'inspect', 'smoke', 'client_probe', 'reload_caddy',
                     'start_stream_probe', 'finish_stream_probe', 'retire_idle_source']:
            patcher = patch.object(deploy_gateway, name, side_effect=getattr(self, 'mock_' + name))
            patcher.start()
            self.addCleanup(patcher.stop)

    def mock_run(self, command, **kwargs):
        self.events.append(('run', command))
        if command[:2] == ['docker', 'cp']:
            Path(command[-1]).write_text('a' * 96 + '  /app/migrations/1_initial.sql\n')
        if 'psql' in command:
            return ('1|' + 'a' * 96 + '|t\n').encode()
        return b''

    def mock_inspect(self, name):
        if name == 'old':
            return {'NetworkSettings': {'Ports': {'8080/tcp': [{'HostPort': '18080'}]},
                    'Networks': {'gateway': {}}},
                    'Config': {'Env': ['DATABASE_URL=postgres://gateway@db-alias/gateway',
                                      'SCHEDULER_PASSTHROUGH=' + ('false' if self.failure == 'queued-mode' else 'true')]}}
        if name == 'database':
            networks = {'gateway': {'IPAddress': '172.18.0.2', 'Aliases': ['db-alias']}}
            if self.failure == 'database-host':
                networks = {'gateway': {'IPAddress': '172.18.0.3', 'Aliases': ['different-db']}}
            return {'NetworkSettings': {'Networks': networks}}
        if name == 'new-image':
            return {'Id': 'sha256:verified'}
        self.events.append(('candidate_running', name))
        return {'State': {'Running': self.failure != 'startup'}}

    def mock_smoke(self, base, revision):
        self.events.append(('smoke', base))
        if self.failure == 'public-smoke' and base == self.args.public_url:
            raise RuntimeError('public version wrong')
        return {'revision': revision}

    def mock_client_probe(self, base, evidence, label):
        self.events.append(('client', label))
        if self.failure == label:
            raise RuntimeError('client failed')

    def mock_retire_idle_source(self, container, port):
        self.assertTrue((Path(self.args.evidence) / 'accepted.json').exists())
        self.events.append(('retire', container))
        return False

    def mock_start_stream_probe(self, base, evidence):
        self.stream_process.poll.return_value = None
        (evidence / 'cutover-stream.jsonl').write_text('before-switch\n')
        self.events.append(('stream', 'start'))
        return self.stream_process

    def mock_finish_stream_probe(self, process, evidence, offset):
        self.assertIs(process, self.stream_process)
        self.assertEqual(offset, len(b'before-switch\n'))
        self.stream_process.poll.return_value = 0
        self.events.append(('stream', 'finish'))
        if self.failure == 'stream':
            raise RuntimeError('stream interrupted')

    def mock_reload_caddy(self, caddy, config, previous, replacement):
        self.assertEqual(config.read_text(), previous)
        self.events.append(('route', replacement))
        config.write_text(replacement)

    def assert_old_not_stopped(self):
        if self.failure:
            self.assertFalse(any(kind == 'retire' for kind, _ in self.events))
        for kind, value in self.events:
            if kind == 'run':
                self.assertNotIn('stop', value)
                self.assertNotIn('kill', value)
                self.assertNotIn('rm', value)

    def test_wrong_database_is_rejected_before_container_creation(self):
        self.failure = 'database-host'
        with self.assertRaisesRegex(RuntimeError, '預檢資料庫'):
            deploy_gateway.deploy(self.args)
        self.assertEqual(self.events, [])
        self.assertEqual(self.config.read_text(), self.previous)

    def test_queued_mode_is_rejected_without_changing_service_or_settings(self):
        self.failure = 'queued-mode'
        with self.assertRaisesRegex(RuntimeError, '排隊模式不可並行'):
            deploy_gateway.deploy(self.args)
        self.assertEqual(self.events, [])
        self.assertEqual(self.config.read_text(), self.previous)

    def test_candidate_startup_failure_preserves_old_and_route(self):
        self.failure = 'startup'
        with self.assertRaises(RuntimeError):
            deploy_gateway.deploy(self.args)
        self.assert_old_not_stopped()
        self.assertEqual(self.config.read_text(), self.previous)
        self.assertTrue(any(kind == 'candidate_running' for kind, _ in self.events))
        self.assertFalse(any(kind in ['client', 'route'] for kind, _ in self.events))

    def test_candidate_client_failure_does_not_switch(self):
        self.failure = 'candidate-client'
        with self.assertRaises(RuntimeError):
            deploy_gateway.deploy(self.args)
        self.assert_old_not_stopped()
        self.assertEqual(self.config.read_text(), self.previous)
        self.assertIn(('client', 'candidate-client'), self.events)
        self.assertFalse(any(kind == 'route' for kind, _ in self.events))

    def test_public_failures_restore_route_and_retain_old(self):
        for failure in ['public-client', 'public-smoke', 'stream']:
            with self.subTest(failure=failure):
                self.args.evidence += '-' + failure
                self.events.clear()
                self.failure = failure
                with self.assertRaises(RuntimeError):
                    deploy_gateway.deploy(self.args)
                self.assert_old_not_stopped()
                self.assertEqual(self.config.read_text(), self.previous)
                routes = [value for kind, value in self.events if kind == 'route']
                self.assertEqual(len(routes), 2)
                self.assertEqual(routes[-1], self.previous)
                self.assertFalse((Path(self.args.evidence) / 'accepted.json').exists())

    def test_success_checks_candidate_before_switch_and_keeps_old(self):
        deploy_gateway.deploy(self.args)
        self.assert_old_not_stopped()
        actual = [kind if kind == 'route' else value for kind, value in self.events
                  if kind in ['client', 'route', 'smoke', 'stream', 'retire']]
        self.assertEqual(actual, ['http://127.0.0.1:18081', 'candidate-client', 'start', 'route',
                                  self.args.public_url, 'public-client', 'finish', 'old'])
        receipt = json.loads((Path(self.args.evidence) / 'accepted.json').read_text())
        self.assertTrue(receipt['accepted'])
        self.assertFalse(receipt['old_stopped'])
        self.assertIn('127.0.0.1:18081', self.config.read_text())


if __name__ == '__main__':
    unittest.main()
