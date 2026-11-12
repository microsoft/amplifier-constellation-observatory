#!/usr/bin/env python3
"""Actual desktop account -> local agent -> verified canvas connection.

Uses only new disposable homes and their UUID-owned OS startup registrations.
The test driver has Python; the downloaded app and installer have no developer
tools on PATH. No model account, model prompt, game or external service is used.
"""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import queue
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.request
import uuid
import zipfile


def remove_stopped_directory(path):
    # Task Scheduler acknowledges End before an owned process releases its last
    # file handle. Wait only for this disposable directory; never ignore failure.
    deadline = time.monotonic() + 20
    while path.exists():
        try:
            shutil.rmtree(path)
            return
        except PermissionError:
            if time.monotonic() >= deadline: raise
            time.sleep(.2)


def unpack(archive, root):
    if archive.name.endswith('.zip'):
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                path = Path(member.filename)
                assert not path.is_absolute() and '..' not in path.parts and not stat.S_ISLNK(member.external_attr >> 16)
                target = root / path; target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.read(member)); target.chmod((member.external_attr >> 16) & 0o777 or 0o644)
    else:
        with tarfile.open(archive) as source:
            for member in source.getmembers():
                path = Path(member.name)
                assert member.isfile() and not path.is_absolute() and '..' not in path.parts
                target = root / path; target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.extractfile(member).read()); target.chmod(member.mode & 0o777)


def cleanup(home):
    if not home or not (home / 'config.json').is_file(): return
    config = json.loads((home / 'config.json').read_text(encoding='utf-8'))
    identifier = str(uuid.UUID(config['config']['instance_id']))
    def command(*args):
        return subprocess.run(args, capture_output=True, timeout=30)
    if os.name == 'nt':
        tool = str(Path(os.environ['SystemRoot']) / 'System32/schtasks.exe')
        for job in ('AgentInstance-' + identifier, 'constellation-updates-' + identifier):
            command(tool, '/End', '/TN', job); command(tool, '/Delete', '/TN', job, '/F')
    elif sys.platform == 'darwin':
        for label in ('io.agent-instance.' + identifier, 'io.constellation-updates-' + identifier):
            command('/bin/launchctl', 'bootout', f'gui/{os.getuid()}/{label}')
            (Path.home() / 'Library/LaunchAgents' / (label + '.plist')).unlink(missing_ok=True)
    else:
        metadata = json.loads((home / 'install.json').read_text(encoding='utf-8'))
        for name in (metadata['service']['name'], 'constellation-updates-' + identifier + '.service'):
            unit = Path.home() / '.config/systemd/user' / name
            if unit.exists():
                assert str(home) in unit.read_text() or identifier in unit.read_text()
                command('/usr/bin/systemctl', '--user', 'disable', '--now', name); unit.unlink()
        command('/usr/bin/systemctl', '--user', 'daemon-reload')
    helper = Path.home() / '.local/share/constellation-updates' / identifier
    if helper.exists():
        registration = json.loads((helper / 'registration.json').read_text(encoding='utf-8'))
        assert registration['instance_id'] == identifier and Path(registration['home']).resolve() == home.resolve()
        remove_stopped_directory(helper)
    time.sleep(1)
    # This home was selected by this test's original frozen request UUID.
    remove_stopped_directory(home)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--metadata', required=True, type=Path)
    args = parser.parse_args()
    record = json.loads(args.metadata.read_text()); assert record['includes_agent_setup']
    archive = args.metadata.parent / record['filename']
    assert hashlib.sha256(archive.read_bytes()).hexdigest() == record['sha256']
    with tempfile.TemporaryDirectory(prefix='Observatory complete café ') as temporary:
        root = Path(temporary).resolve(); download = root / 'download'; download.mkdir()
        unpack(archive, download); binary = download / record['entry']
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == record['binary_sha256']
        home = root / 'Observatory 日本'; errors = root / 'stderr.txt'
        env = dict(os.environ, PYTHONHOME=str(root / 'not-python'), PYTHONPATH=str(root / 'not-python'))
        if os.name == 'nt':
            system = Path(os.environ['SystemRoot']) / 'System32'
            env['PATH'] = str(system) + os.pathsep + str(system / 'WindowsPowerShell/v1.0')
        else:
            tools = root / 'os-tools'; tools.mkdir()
            for name in ('id', 'hostname', 'launchctl' if sys.platform == 'darwin' else 'systemctl'):
                executable = shutil.which(name); assert executable, name
                (tools / name).symlink_to(executable)
            env['PATH'] = str(tools)
        assert all(shutil.which(name, path=env['PATH']) is None for name in ('python', 'python3', 'node', 'cargo', 'rustc', 'git'))
        process = None; agent_home = None
        try:
            with errors.open('w', encoding='utf-8') as stderr:
                process = subprocess.Popen([str(binary), '--home', str(home), '--no-browser'], env=env,
                                           stdout=subprocess.PIPE, stderr=stderr, text=True, encoding='utf-8')
                lines = queue.Queue(); threading.Thread(target=lambda: lines.put(process.stdout.readline()), daemon=True).start()
                announced = json.loads(lines.get(timeout=30)); origin, capability = announced['url'].split('#'); origin = origin.rstrip('/')
                browser = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
                csrf = None
                def request(path, body=None, setup=False):
                    headers = {'Origin': origin}
                    if body is not None: headers['Content-Type'] = 'application/json'
                    if setup: headers['Authorization'] = 'Bearer ' + capability
                    if csrf: headers['X-CSRF-Token'] = csrf
                    req = urllib.request.Request(origin + path, headers=headers, data=json.dumps(body).encode() if body is not None else None)
                    with browser.open(req, timeout=180) as response: return json.load(response)
                account = request('/desktop/create', {'request_id': str(uuid.uuid4()), 'name': 'Native café 日本',
                    'password': 'Disposable native check ' + uuid.uuid4().hex, 'start_at_signin': False}, True)
                assert account['authenticated']; csrf = request('/auth/session')['csrf']
                hosts = request('/api/v1/hosts'); assert len(hosts) == 1 and hosts[0]['native_local']
                body = {'host_id': hosts[0]['id'], 'recipe_id': 'bundled-instance', 'request_id': str(uuid.uuid4()), 'name': 'Robot café 日本'}
                plan = request('/api/v1/install/plan', body); agent_home = Path(plan['home'])
                assert agent_home.name == body['request_id'] and not agent_home.exists()
                body['plan_digest'] = plan['plan_digest']; request('/api/v1/install', body)
                deadline = time.monotonic() + 330
                while time.monotonic() < deadline:
                    job = request('/api/v1/jobs/' + body['request_id'])
                    if job['status'] not in ('queued', 'running'): break
                    time.sleep(1)
                assert job['status'] == 'completed' and job.get('endpoint_id') == body['request_id'], job.get('note')
                endpoints = request('/api/v1/endpoints')
                assert len(endpoints) == 1 and endpoints[0]['id'] == body['request_id']
                before = {name: (agent_home / name).read_bytes() for name in ('config.json', 'secrets/control-token', 'tls/root-ca.crt')}
                repeated = request('/api/v1/install', body); assert repeated['id'] == job['id']
                assert request('/api/v1/endpoints') == endpoints
                for name, value in before.items(): assert (agent_home / name).read_bytes() == value
                manifest = json.loads((home / 'support/instance/consumer-bundle.json').read_text())
                installed = json.loads((agent_home / 'engine/installation.json').read_text())
                assert installed['sha256'] == manifest['native_runtime']['binary_sha256'] and not installed['credentials_copied']
                print(json.dumps({'passed': ['native first account', 'this computer appears', 'fixed local agent setup',
                    'private Python and published Native engine', 'actual current-user service startup',
                    'authenticated identity automatically connected', 'same request preserves one agent and original data'],
                    'target': record['target'], 'instance_version': manifest['version'], 'model_calls': 0,
                    'limits': ['CI user account; no external account authorization', 'no clean retail-machine claim']}))
        except Exception:
            if errors.exists(): print(errors.read_text(encoding='utf-8', errors='replace')[-4000:], file=sys.stderr)
            if agent_home:
                diagnostic = home / 'jobs' / (agent_home.name + '.stderr')
                if diagnostic.exists(): print(diagnostic.read_text(encoding='utf-8', errors='replace')[-4000:], file=sys.stderr)
            raise
        finally:
            if process:
                process.terminate()
                try: process.wait(timeout=10)
                except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=10)
                process.stdout.close()
            cleanup(agent_home)


if __name__ == '__main__': main()
