#!/usr/bin/env python3
"""Pure fixture tests: no native executable, service, network or model runs."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location('native_launch', Path(__file__).with_name('check-native-launch.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def phase(name='agent_support', state='started'):
    return {'schema': 1, 'phase': name, 'state': state, 'elapsed_ms': 250, 'phase_ms': 0}


class DiagnosticsTests(unittest.TestCase):
    def test_create_timeout_records_last_fixed_phase_before_cleanup_without_secrets(self):
        child = SimpleNamespace(returncode=None)
        child.poll = lambda: child.returncode
        calls = []
        state = {}
        secret = 'private test token never appears in retained diagnostics'

        def terminate():
            calls.append('terminate')
            child.returncode = 1
        child.terminate = terminate
        child.wait = lambda timeout=None: child.returncode
        child.kill = lambda: self.fail('The already-settled fixture child must not be killed again')

        def spawn(argv, **kwargs):
            state['home'] = Path(argv[argv.index('--home') + 1])
            state['stderr'] = kwargs['stderr']
            child.stdout = io.StringIO(json.dumps({'state': 'setup', 'url': 'http://127.0.0.1:12345/#' + secret}) + '\n')
            calls.append('spawn')
            return child

        class Response:
            status = 200
            headers = {}
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def read(self): return b'fixture asset'

        def open_request(request, timeout):
            self.assertEqual(timeout, 20)
            path = request.selector
            if path == '/desktop/status':
                code = 403 if request.get_header('Origin') == 'https://unrelated.invalid' else 401
                raise urllib.error.HTTPError(request.full_url, code, 'fixture', {}, io.BytesIO(b''))
            if path == '/desktop/create':
                body = json.loads(request.data)
                self.assertFalse(body['start_at_signin'])
                state['password'] = body['password']
                staged = state['home'].parent / ('.observatory-install-' + body['request_id'])
                (staged / 'secrets').mkdir(parents=True)
                (staged / 'secrets/local-password.json').write_text(secret)
                (staged / 'support/instance').mkdir(parents=True)
                state['stderr'].write(secret + '\nOBSERVATORY_SETUP ' + json.dumps(phase()) + '\n')
                state['stderr'].flush()
                raise TimeoutError('Original fixed20s create failure')
            return Response()

        capture = io.StringIO()
        with patch.object(subprocess, 'run', return_value=SimpleNamespace(stdout='agent-observatory 0.12.0\n')), \
             patch.object(subprocess, 'Popen', side_effect=spawn), \
             patch.object(fixture.urllib.request, 'build_opener', return_value=SimpleNamespace(open=open_request)), \
             contextlib.redirect_stderr(capture):
            with self.assertRaises(TimeoutError):
                fixture.run(Path('not-executed'), '0.12.0')
        self.assertEqual(calls, ['spawn', 'terminate'])
        self.assertFalse(state['home'].parent.exists())
        line = capture.getvalue().strip()
        self.assertTrue(line.startswith('OBSERVATORY_SETUP_EVIDENCE '))
        record = json.loads(line.removeprefix('OBSERVATORY_SETUP_EVIDENCE '))
        self.assertEqual(record['phase_records'], [phase()])
        self.assertEqual(record['staged_observation']['password_record'], 'file')
        self.assertEqual(record['staged_observation']['instance_directory'], 'directory')
        self.assertEqual(record['staged_observation']['instance_manifest'], 'absent')
        self.assertEqual(record['home_observation']['root'], 'absent')
        self.assertEqual(record['child_returncode'], 1)
        for value in (secret, state['password'], str(state['home'])):
            self.assertNotIn(value, line)

    def test_evidence_rejects_non_fixed_fields_and_preserves_bounded_valid_events(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); log = root / 'stderr.txt'
            records = [dict(phase(), secret='do not retain'), phase('private/path'), dict(phase(), phase_ms=-1),
                       dict(phase(), elapsed_ms=True), {'unexpected': 'private payload'}, phase(state='arbitrary')]
            log.write_text('\n'.join('OBSERVATORY_SETUP ' + json.dumps(value) for value in records + [phase()] * 100))
            evidence = fixture.setup_evidence(root / 'missing', None, log, None)
            self.assertEqual(evidence['phase_records'], [phase()] * 64)
            self.assertEqual(evidence['staged_observation'], {'selected': False})
            self.assertIsNone(evidence['child_returncode'])
            self.assertNotIn('private payload', json.dumps(evidence))
            self.assertNotIn('private/path', json.dumps(evidence))

    def test_missing_log_does_not_manufacture_a_phase_or_successful_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            evidence = fixture.setup_evidence(root / 'missing', None, root / 'missing.log', None)
            self.assertEqual(evidence['phase_records'], [])
            self.assertEqual(evidence['phase_read_error'], 'FileNotFoundError')
            self.assertEqual(evidence['home_observation']['root'], 'absent')
            self.assertNotIn('cleanup_confirmed', evidence)


if __name__ == '__main__':
    unittest.main()
