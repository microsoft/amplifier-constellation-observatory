#!/usr/bin/env python3
"""Run the packaged native app in a disposable home; no agents, pairing or OS startup."""
import argparse, hashlib, http.cookiejar, json, os, queue, stat, subprocess, sys, tarfile, tempfile, threading, time, urllib.error, urllib.parse, urllib.request, uuid, zipfile
from pathlib import Path

SETUP_PHASES = frozenset(("staging", "password", "config", "staged_database", "agent_support",
    "application_support", "publish", "service_lock", "final_database", "startup", "setup_receipt",
    "runtime_receipt", "sign_in"))


def setup_evidence(home, staged, stderr, process):
    """Content-free observations before the fixture removes its own paths."""
    phases = []
    read_error = None
    try:
        with stderr.open('rb') as stream:
            stream.seek(max(0, stderr.stat().st_size - 65536))
            for line in stream.read(65536).decode('utf-8', errors='replace').splitlines():
                if not line.startswith('OBSERVATORY_SETUP '): continue
                try:
                    record = json.loads(line[len('OBSERVATORY_SETUP '):])
                    if (set(record) == {'schema', 'phase', 'state', 'elapsed_ms', 'phase_ms'}
                        and record['schema'] == 1 and record['phase'] in SETUP_PHASES
                        and record['state'] in ('started', 'finished', 'failed')
                        and all(type(record[key]) is int and 0 <= record[key] <= 2**63-1
                                for key in ('elapsed_ms', 'phase_ms'))):
                        phases.append(record)
                except (ValueError, TypeError, KeyError): pass
    except OSError as error:
        read_error = type(error).__name__
    def kind(path):
        try:
            metadata = path.lstat()
            mode = metadata.st_mode
            linked = stat.S_ISLNK(mode) or bool(getattr(metadata, 'st_file_attributes', 0) & getattr(stat, 'FILE_ATTRIBUTE_REPARSE_POINT', 0))
            return 'link' if linked else 'directory' if stat.S_ISDIR(mode) else 'file' if stat.S_ISREG(mode) else 'other'
        except FileNotFoundError: return 'absent'
        except OSError: return 'unreadable'
    def snapshot(root):
        if root is None: return {'selected': False}
        names = {'root': '.', 'password_record': 'secrets/local-password.json', 'config': 'config.json',
                 'database': 'state.sqlite3', 'instance_directory': 'support/instance',
                 'instance_manifest': 'support/instance/consumer-bundle.json',
                 'helper_manifest': 'machine-helper-artifacts/manifest.json',
                 'app_receipt': 'app-install.json', 'setup_receipt': 'desktop-setup.json'}
        return {label: kind(root / name) for label, name in names.items()}
    return {'schema': 1, 'kind': 'observatory-native-setup-diagnostics',
            'child_returncode': process.poll() if process is not None else None,
            'phase_records': phases[-64:], 'phase_read_error': read_error,
            'home_observation': snapshot(home), 'staged_observation': snapshot(staged),
            'limits': 'Fixed-path observations and bounded fixed phase records; no contents, recursive counts or cleanup proof.'}


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:process.wait(timeout=10)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=10)

def run(binary, expected_version):
    version=subprocess.run([str(binary),'--version'],capture_output=True,text=True,check=True,timeout=20).stdout.strip()
    assert version==f'agent-observatory {expected_version}',version
    with tempfile.TemporaryDirectory(prefix='observatory native launch ') as temp:
        home=Path(temp)/'My Observatory'; stderr=Path(temp)/'stderr.txt'; process=None; staged=None
        try:
            with stderr.open('w') as errors:
                process=subprocess.Popen([str(binary),'--home',str(home),'--no-browser'],stdout=subprocess.PIPE,stderr=errors,text=True)
                lines=queue.Queue();threading.Thread(target=lambda:lines.put(process.stdout.readline()),daemon=True).start()
                try:line=lines.get(timeout=30)
                except queue.Empty:raise AssertionError('Native setup did not announce a local page')
                if not line:raise AssertionError('Native setup exited: '+stderr.read_text())
                initial=json.loads(line);assert initial['state']=='setup'
                parsed=urllib.parse.urlsplit(initial['url']);assert parsed.scheme=='http' and parsed.hostname=='127.0.0.1'
                origin=f'{parsed.scheme}://{parsed.netloc}';token=parsed.fragment
                jar=http.cookiejar.CookieJar(); browser=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
                def request(path,method='GET',body=None,capability=False,extra=None):
                    headers={'Origin':origin}
                    if body is not None:headers['Content-Type']='application/json'
                    if capability:headers['Authorization']='Bearer '+token
                    if extra:headers.update(extra)
                    req=urllib.request.Request(origin+path,data=json.dumps(body).encode() if body is not None else None,headers=headers,method=method)
                    try:
                        with browser.open(req,timeout=20) as response:return response.status,response.read(),dict(response.headers)
                    except urllib.error.HTTPError as error:return error.code,error.read(),dict(error.headers)
                assert request('/')[0]==200
                for asset in ['/desktop/setup.js','/desktop/setup.css','/desktop/nunito.woff2','/desktop/frame.webp']:assert request(asset)[0]==200,asset
                assert request('/desktop/status')[0]==401
                assert request('/desktop/status',capability=True,extra={'Origin':'https://unrelated.invalid'})[0]==403
                password='Disposable native check '+str(uuid.uuid4())
                body={'request_id':str(uuid.uuid4()),'name':'Native Test','password':password,'start_at_signin':False}
                staged=home.parent/('.observatory-install-'+body['request_id'])
                status,data,_=request('/desktop/create','POST',body,True);assert status==200,(status,data.decode())
                assert json.loads(data)['authenticated'] is True
                assert json.loads(request('/desktop/status',capability=True)[1])['ready'] is True
                assert json.loads(request('/auth/session')[1])['authenticated'] is True
                assert request('/')[0]==200
                cfg=(home/'config.json').read_bytes();identity=json.loads(cfg)['id']
                secret=(home/'secrets/local-password.json').read_bytes();assert password.encode() not in secret
                assert json.loads(cfg)['bind']=='127.0.0.1'
                assert json.loads(cfg)['web_transport']=='local-http'
                receipt=json.loads((home/'desktop-setup.json').read_text());assert receipt['startup_enabled'] is False
                repeated=subprocess.run([str(binary),'--home',str(home),'--no-browser'],capture_output=True,text=True,timeout=20,check=True)
                assert json.loads(repeated.stdout)['id']==identity
                assert (home/'config.json').read_bytes()==cfg
                assert (home/'secrets/local-password.json').read_bytes()==secret
                stop(process)
                process=subprocess.Popen([str(binary),'--home',str(home),'--no-browser'],stdout=subprocess.DEVNULL,stderr=errors)
                jar.clear()
                for _ in range(150):
                    if process.poll() is not None:raise AssertionError('Native reopen failed: '+stderr.read_text())
                    try:
                        if request('/auth/session')[0]==200:break
                    except (OSError,urllib.error.URLError):pass
                    time.sleep(.1)
                else:raise AssertionError('Native reopen did not answer')
                status,data,_=request('/auth/login','POST',{'username':'Native Test','password':password})
                assert status==200 and json.loads(data)['authenticated'] is True,(status,data.decode())
                assert (home/'config.json').read_bytes()==cfg
                assert (home/'secrets/local-password.json').read_bytes()==secret
                print(json.dumps({'version':expected_version,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'native_os':os.name,'passed':['packaged executable','local setup page and assets','setup capability boundary','local account creation','password stored hashed','reopen existing instance','native process restart and login','configuration and password unchanged'],'startup_operations':0,'agents_created':0,'model_calls':0}))
        finally:
            try:
                if process is not None:stop(process)
            finally:
                # Read only the fixed diagnostic fields before TemporaryDirectory
                # removes this fixture's stage; never emit raw stderr/state.
                print('OBSERVATORY_SETUP_EVIDENCE '+json.dumps(setup_evidence(home,staged,stderr,process)),file=sys.stderr,flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--metadata',type=Path,required=True);a=p.parse_args();record=json.loads(a.metadata.read_text());archive=a.metadata.parent/record['filename']
    assert hashlib.sha256(archive.read_bytes()).hexdigest()==record['sha256']
    with tempfile.TemporaryDirectory(prefix='Observatory app download ') as temp:
        root=Path(temp);entry=Path(record['entry']);assert not entry.is_absolute() and '..' not in entry.parts
        binary=root/entry;binary.parent.mkdir(parents=True,exist_ok=True)
        if record['os']=='linux':
            with tarfile.open(archive) as z:
                for member in z.getmembers():
                    path=Path(member.name);assert member.isfile() and not path.is_absolute() and '..' not in path.parts
                    target=root/path;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(z.extractfile(member).read());target.chmod(member.mode & 0o777)
        else:
            with zipfile.ZipFile(archive) as z:
                for item in z.infolist():
                    path=Path(item.filename);assert not path.is_absolute() and '..' not in path.parts and not stat.S_ISLNK(item.external_attr>>16)
                    target=root/path;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(z.read(item));target.chmod((item.external_attr>>16)&0o777 or 0o644)
        binary.chmod(0o755);assert hashlib.sha256(binary.read_bytes()).hexdigest()==record['binary_sha256']
        run(binary.resolve(),record['version'])
