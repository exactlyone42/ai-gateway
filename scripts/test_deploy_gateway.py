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
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.evidence = Path(self.temp.name)
        self.process = Mock()
        self.process.wait.return_value = 0
        self.start = {'type': 'stream_event', 'event': {'type': 'message_start'}}
        self.delta = {'type': 'stream_event', 'event': {'type': 'content_block_delta',
                      'delta': {'type': 'text_delta', 'text': 'GATEWAY_STREAM_END'}}}
        self.terminal = {'type': 'stream_event', 'event': {'type': 'message_delta',
                         'delta': {'stop_reason': 'end_turn'}}}
        self.stop = {'type': 'stream_event', 'event': {'type': 'message_stop'}}
        self.result = {'type': 'result', 'is_error': False, 'num_turns': 1,
                       'result': 'GATEWAY_STREAM_END'}

    def write(self, records):
        output = b''.join(json.dumps(item).encode() + b'\n' for item in records)
        (self.evidence / 'cutover-stream.jsonl').write_bytes(output)
        return output

    def test_content_must_continue_after_switch_offset(self):
        output = self.write([self.start, self.delta, self.terminal, self.stop, self.result])
        offset = len(json.dumps(self.start).encode()) + 1
        deploy_gateway.finish_stream_probe(self.process, self.evidence, offset)
        with self.assertRaisesRegex(RuntimeError, '切換後沒有新的文字串流內容'):
            deploy_gateway.finish_stream_probe(self.process, self.evidence, len(output))

    def test_cutoff_then_successful_retry_is_rejected(self):
        resumed = dict(self.result, num_turns=2)
        self.write([self.start, self.delta, self.stop,
                    {'type': 'user', 'message': {'content': 'cut off mid-stream'}},
                    self.start, self.delta, self.terminal, self.stop, resumed])
        with self.assertRaisesRegex(RuntimeError, '續傳或重寫'):
            deploy_gateway.finish_stream_probe(self.process, self.evidence, 0)

    def test_hook_rewrite_is_also_rejected(self):
        self.write([self.start, self.delta, self.terminal, self.stop,
                    {'type': 'user', 'message': {'content': 'Stop hook feedback'}}, self.result])
        with self.assertRaisesRegex(RuntimeError, '續傳或重寫'):
            deploy_gateway.finish_stream_probe(self.process, self.evidence, 0)

    def test_missing_or_abnormal_terminal_is_rejected(self):
        for terminal in [None, {'type': 'stream_event', 'event': {
                'type': 'message_delta', 'delta': {'stop_reason': 'max_tokens'}}}]:
            with self.subTest(terminal=terminal):
                events = [self.start, self.delta]
                if terminal:
                    events.append(terminal)
                self.write(events + [self.stop, self.result])
                with self.assertRaisesRegex(RuntimeError, '正常結束事件'):
                    deploy_gateway.finish_stream_probe(self.process, self.evidence, 0)

    def test_thinking_after_cutover_is_not_text_progress(self):
        prefix = self.write([self.start, self.delta])
        thinking = {'type': 'stream_event', 'event': {'type': 'content_block_delta',
                    'delta': {'type': 'thinking_delta', 'thinking': 'GATEWAY_STREAM_END'}}}
        self.write([self.start, self.delta, thinking, self.terminal, self.stop, self.result])
        with self.assertRaisesRegex(RuntimeError, '切換後沒有新的文字串流內容'):
            deploy_gateway.finish_stream_probe(self.process, self.evidence, len(prefix))

    def test_multiple_messages_without_synthetic_user_are_rejected(self):
        self.write([self.start, self.delta, self.terminal, self.stop,
                    self.start, self.delta, self.terminal, self.stop, self.result])
        with self.assertRaisesRegex(RuntimeError, '正常結束事件'):
            deploy_gateway.finish_stream_probe(self.process, self.evidence, 0)


class RetirementTests(unittest.TestCase):
    def test_shutdown_wait_covers_custom_deadlines(self):
        self.assertEqual(deploy_gateway.shutdown_wait({}), 60)
        self.assertEqual(deploy_gateway.shutdown_wait({'DRAIN_TIMEOUT_MS': ' ', 'SHUTDOWN_FINALIZE_TIMEOUT_MS': ''}), 60)
        self.assertEqual(deploy_gateway.shutdown_wait({'DRAIN_TIMEOUT_MS': '600000', 'SHUTDOWN_FINALIZE_TIMEOUT_MS': '60000'}), 795)
        for env in ({'DRAIN_TIMEOUT_MS': '600001'}, {'SHUTDOWN_FINALIZE_TIMEOUT_MS': '0'}, {'DRAIN_TIMEOUT_MS': 'invalid'}):
            with self.subTest(env=env), self.assertRaises(RuntimeError):
                deploy_gateway.shutdown_wait(env)

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

        def run(command, **kwargs):
            if 'stop' in command:
                self.assertEqual(command, ['docker', 'stop', '--time', '60', 'old'])
            events.append('stop' if 'stop' in command else 'tcp')
            return b''

        with patch.object(deploy_gateway, 'fetch', side_effect=fetch), patch.object(deploy_gateway, 'run', side_effect=run), patch.object(deploy_gateway, 'inspect', return_value={'State': {'Status': 'exited', 'ExitCode': 0}}), patch.object(deploy_gateway.time, 'sleep'):
            self.assertTrue(deploy_gateway.retire_idle_source('old', '18080'))
        self.assertEqual(events, ['health', 'tcp'] * 3 + ['stop'])

    def test_forced_or_unknown_exit_is_not_reported_as_clean(self):
        for state in ({'Status': 'exited', 'ExitCode': 137}, {'Status': 'running', 'ExitCode': 0}, {'Status': 'exited'}):
            with self.subTest(state=state), patch.object(deploy_gateway, 'fetch', return_value={'active_requests': 0}), patch.object(deploy_gateway, 'run', return_value=b''), patch.object(deploy_gateway, 'inspect', return_value={'State': state}), patch.object(deploy_gateway.time, 'sleep'):
                with self.assertRaises(RuntimeError):
                    deploy_gateway.retire_idle_source('old', '18080')


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
                                      'SCHEDULER_PASSTHROUGH=' + ('false' if self.failure == 'queued-mode' else 'true'),
                                      *getattr(self, 'extra_source_env', [])]}}
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

    def mock_retire_idle_source(self, container, port, wait_seconds):
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

    def test_codex_owner_is_required_before_starting_candidate_and_preserved(self):
        owner = '00000000-0000-0000-0000-000000000001'
        for index, (requested, stored, valid) in enumerate([
            (None, None, False), ('invalid', None, False),
            (owner, None, True), (None, owner, True),
        ]):
            with self.subTest(requested=requested, stored=stored):
                self.args.evidence = str(Path(self.temp.name) / f'owner-{index}')
                self.args.codex_owner_user_id = requested
                self.extra_source_env = ['CODEX_AUTH_FILE=/run/gateway-codex/auth.json']
                if stored:
                    self.extra_source_env.append('CODEX_OWNER_USER_ID=' + stored)
                self.events.clear()
                self.config.write_text(self.previous)
                if not valid:
                    with self.assertRaisesRegex(RuntimeError, '訂閱持有人'):
                        deploy_gateway.deploy(self.args)
                    self.assertEqual(self.events, [])
                    self.assertEqual(self.config.read_text(), self.previous)
                else:
                    deploy_gateway.deploy(self.args)
                    env = (Path(self.args.evidence) / 'candidate.env').read_text()
                    self.assertIn('CODEX_OWNER_USER_ID=' + owner, env.splitlines())

    def test_created_resources_have_expiry_labels(self):
        deploy_gateway.deploy(self.args)
        commands = [value for kind, value in self.events if kind == 'run']
        created = [command for command in commands if command[:2] in [['docker', 'create'], ['docker', 'run']] or command[:3] == ['docker', 'volume', 'create']]
        self.assertEqual(len(created), 3)
        for command in created:
            self.assertIn('--label', command)
            self.assertTrue(command[command.index('--label') + 1].startswith('expire='))

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


class CredentialMountTests(unittest.TestCase):
    def test_preserves_existing_credentials_and_replaces_only_spool(self):
        source = {'Mounts': [{'Source': '/held', 'Destination': '/held', 'RW': True}, {'Source': '/old-spool', 'Destination': '/var/lib/agent-pool-gateway/ledger-spool', 'RW': True}]}
        result = deploy_gateway.credential_mounts(source)
        self.assertIn('type=bind,src=/held,dst=/held', result)
        self.assertFalse(any('/old-spool' in item for item in result))
        with tempfile.TemporaryDirectory() as root:
            self.assertIn('type=bind,src=' + root + ',dst=/run/gateway-codex,readonly', deploy_gateway.credential_mounts(source, root))
