#!/usr/bin/env python3
"""Actual two-build desktop replacement in a disposable home; no model or agent calls.

The second executable differs only by fixture bytes (an ad-hoc signature
identifier on macOS, inert trailing bytes elsewhere). This checks
process identity/hash, update and preserved state; it is not release lineage proof.
"""
import argparse, hashlib, json, os, shutil, sqlite3, subprocess, tempfile, time, urllib.request, urllib.parse, uuid
from pathlib import Path
from contextlib import closing

def digest(p): return hashlib.sha256(p.read_bytes()).hexdigest()
def fixture_binary(source, executable, marker):
    mac = os.name!='nt' and os.uname().sysname=='Darwin'
    executable.write_bytes(source.read_bytes() + (b'' if mac else marker))
    executable.chmod(0o700)
    if mac:
        # Appending outside Mach-O segments makes codesign reject the file.
        # A distinct local signature identifier varies the hash without changing
        # executable behavior. This is not publisher/notarization proof.
        result=subprocess.run(['/usr/bin/codesign','--force','--sign','-',
            '--identifier','org.constellation.fixture.'+hashlib.sha256(marker).hexdigest(),str(executable)],
            stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        assert result.returncode==0,result.stderr
        subprocess.run(['/usr/bin/codesign','--verify',str(executable)],check=True)

def request(url,token,path,body=None):
    r=urllib.request.Request(url+path,data=json.dumps(body).encode() if body is not None else None,
        headers={'Authorization':'Bearer '+token,**({'Content-Type':'application/json'} if body is not None else {})})
    with urllib.request.urlopen(r,timeout=90) as response: return json.load(response)
def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',required=True,type=Path);a=p.parse_args();binary=a.binary.resolve()
    target=('aarch64' if os.uname().machine in ('aarch64','arm64') else 'x86_64') if os.name!='nt' else ('aarch64' if os.environ.get('PROCESSOR_ARCHITECTURE','').lower()=='arm64' else 'x86_64')
    target+='-pc-windows-msvc' if os.name=='nt' else '-apple-darwin' if os.uname().sysname=='Darwin' else '-unknown-linux-gnu'
    version=subprocess.check_output([str(binary),'--version'],text=True).strip().split()[-1];name=binary.name
    checks=[];processes=[];offers=[];offer_handles=[];home=None;origin=None;control=None;exit_handle=None
    instance_name='agent-instance.exe' if os.name=='nt' else 'agent-instance'
    def package(directory,executable,support_version):
        support=directory/'support/instance';support.mkdir(parents=True)
        (support/instance_name).write_bytes(('inert setup '+support_version).encode())
        manifest={'schema':1,'kind':'instance-consumer-bundle','version':support_version,'target':target,'binary':instance_name,'native_runtime':{},'files':{instance_name:digest(support/instance_name)}}
        (support/'consumer-bundle.json').write_text(json.dumps(manifest,indent=2)+'\n')
        (directory/'observatory-package.json').write_text(json.dumps({'schema':1,'component':'agent-observatory','target':target,'version':version,'binary':name,'binary_sha256':digest(executable),'instance_manifest_sha256':digest(support/'consumer-bundle.json')}))
    with tempfile.TemporaryDirectory(prefix='observatory update café ') as folder:
        root=Path(folder);home=root/'same individual';old=root/'old';new=root/'new';old.mkdir();new.mkdir()
        for directory,extra,support_version in [(old,b'','fixture-old'),(new,b'\nobservatory-inert-update-fixture\n','fixture-new')]:
            executable=directory/name;fixture_binary(binary,executable,extra)
            package(directory,executable,support_version)
        def launch(executable):
            proc=subprocess.Popen([str(executable),'--home',str(home),'--no-browser'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8');processes.append(proc)
            line=proc.stdout.readline();assert line,(proc.poll(),proc.stderr.read());value=json.loads(line);u=urllib.parse.urlsplit(value['url']);address=u.scheme+'://'+u.netloc
            if value.get('state')=='update_available':
                offers.append((address,u.fragment))
                if os.name=='nt':
                    import ctypes
                    api=ctypes.WinDLL('kernel32',use_last_error=True)
                    api.OpenProcess.restype=ctypes.c_void_p
                    api.OpenProcess.argtypes=[ctypes.c_uint32,ctypes.c_bool,ctypes.c_uint32]
                    api.WaitForSingleObject.argtypes=[ctypes.c_void_p,ctypes.c_uint32]
                    api.CloseHandle.argtypes=[ctypes.c_void_p]
                    handle=api.OpenProcess(0x00100000,False,value['pid']);assert handle,ctypes.get_last_error()
                    offer_handles.append((api,handle))
            return proc,address,u.fragment
        try:
            previous,origin,setup_token=launch(old/name)
            request(origin,setup_token,'/desktop/create',{'request_id':str(uuid.uuid4()),'name':'Update fixture','password':'Disposable test password 938!','start_at_signin':False})
            control=(home/'secrets/control-token').read_text().strip();initial=request(origin,control,'/api/v1/status')
            saved={name:(home/name).read_bytes() for name in ('config.json','secrets/control-token','app-install.json')}
            with closing(sqlite3.connect(home/'state.sqlite3')) as db:
                db.execute('CREATE TABLE fixture_unknown(receipt TEXT)');db.execute('INSERT INTO fixture_unknown VALUES (?)',('unknown world effect: never replay',));db.commit()
            assert (home/'bin'/name).is_file();checks.append('private executable retained with sign-in startup disabled')
            candidate,update_url,update_token=launch(new/name)
            assert request(update_url,update_token,'/status')['phase']=='prepared'
            original_update=json.loads((home/'desktop-update.json').read_text())['request_id']
            candidate.terminate();candidate.wait(timeout=15)
            candidate,update_url,update_token=launch(home/'bin'/name)
            assert json.loads((home/'desktop-update.json').read_text())['request_id']==original_update
            assert request(update_url,update_token,'/status')['phase']=='prepared'
            checks.append('reopening the retained application resumes the same pending request without a package or duplicate install')
            if os.name=='nt':
                candidate.wait(timeout=15)
                assert candidate.returncode==0,'Installed launcher must exit before Windows replaces its mapped image'
                assert request(update_url,update_token,'/status')['phase']=='prepared'
                checks.append('installed Windows launcher hands update window to verified private copy and exits before replacement')
            request(update_url,update_token,'/apply',{})
            deadline=time.monotonic()+90
            while time.monotonic()<deadline:
                state=request(update_url,update_token,'/status')
                if state['phase'] in ('applied','rolled_back','deferred'):break
                time.sleep(.2)
            assert state['phase']=='applied',state
            actual=request(origin,control,'/api/v1/status');assert actual['id']==initial['id'];assert actual['binary_sha256']==digest(new/name);assert actual['pid']!=previous.pid
            assert (home/'support/instance/consumer-bundle.json').read_bytes()==(new/'support/instance/consumer-bundle.json').read_bytes()
            previous.wait(timeout=15);checks.append('new actual process PID and exact binary confirmed at same authenticated installation')
            # Replay only the same update receipt: no replacement or process restart.
            first_pid=actual['pid'];request(update_url,update_token,'/apply',{});time.sleep(.3)
            assert request(origin,control,'/api/v1/status')['pid']==first_pid;checks.append('same request retry settles without a second replacement')
            for n in ('config.json','secrets/control-token'):assert (home/n).read_bytes()==saved[n]
            with closing(sqlite3.connect(home/'state.sqlite3')) as db:assert db.execute('SELECT receipt FROM fixture_unknown').fetchall()==[('unknown world effect: never replay',)]
            checks.append('account identity, address, credential and retained unknown receipt preserved')
            # Inject an invalid expected startup version after a second honest package has
            # staged. The actual candidate process must exit; rollback must retain the
            # previously installed binary and state, rather than claim a free lock is exit.
            bad=root/'failing candidate';bad.mkdir();bad_binary=bad/name
            fixture_binary(binary,bad_binary,b'\nfixture-startup-rejection\n')
            package(bad,bad_binary,'fixture-bad')
            bad_offer,bad_url,bad_token=launch(bad_binary)
            j=json.loads((home/'desktop-update.json').read_text());j['candidate']['version']='fixture-rejected-startup'
            (home/'desktop-update.json').write_text(json.dumps(j))
            request(bad_url,bad_token,'/apply',{})
            deadline=time.monotonic()+90
            while time.monotonic()<deadline:
                failed=request(bad_url,bad_token,'/status')
                if failed['phase']=='rolled_back':break
                time.sleep(.2)
            assert failed['phase']=='rolled_back',failed
            restored=request(origin,control,'/api/v1/status');assert restored['binary_sha256']==digest(new/name)
            assert restored['id']==initial['id'];assert restored['pid']!=first_pid;first_pid=restored['pid']
            assert json.loads((home/'app-install.json').read_text())['binary_sha256']==digest(new/name)
            with closing(sqlite3.connect(home/'state.sqlite3')) as db:assert db.execute('SELECT receipt FROM fixture_unknown').fetchall()==[('unknown world effect: never replay',)]
            checks.append('actual rejected candidate exits before previous executable restarts; unknown receipt remains unchanged')
            bad_offer.terminate();bad_offer.wait(timeout=15)
            assert (home/'support/instance/consumer-bundle.json').read_bytes()==(new/'support/instance/consumer-bundle.json').read_bytes()
            checks.append('future setup support and only its owned recipe update together, then rollback together; manifest bytes preserved')
            shutil.rmtree(old);candidate.terminate();candidate.wait(timeout=15);shutil.rmtree(new)
            assert request(origin,control,'/api/v1/status')['pid']==first_pid;checks.append('installed application remains running after both download folders are removed')
        finally:
            for address,token in offers:
                try:request(address,token,'/close',{})
                except (OSError,ValueError):pass
            if home and origin and control and (home/'desktop-update.json').exists():
                try:
                    if os.name=='nt':
                        import ctypes
                        current=request(origin,control,'/api/v1/status');assert current['id']==initial['id']
                        kernel=ctypes.WinDLL('kernel32',use_last_error=True)
                        kernel.OpenProcess.restype=ctypes.c_void_p
                        kernel.OpenProcess.argtypes=[ctypes.c_uint32,ctypes.c_bool,ctypes.c_uint32]
                        kernel.WaitForSingleObject.argtypes=[ctypes.c_void_p,ctypes.c_uint32]
                        kernel.CloseHandle.argtypes=[ctypes.c_void_p]
                        exit_handle=kernel.OpenProcess(0x00100000,False,current['pid'])
                        assert exit_handle,ctypes.get_last_error()
                    j=json.loads((home/'desktop-update.json').read_text());j['phase']='recovering';(home/'desktop-update.json').write_text(json.dumps(j))
                    request(origin,control,'/api/v1/native-update/stop',{'request_id':j['request_id'],'version':version})
                except Exception:pass
            for process in processes:
                if process.poll() is None:process.terminate()
                try:process.wait(timeout=15)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=15)
            for api,handle in offer_handles:
                try:assert api.WaitForSingleObject(handle,15000)==0,'Owned update window did not exit'
                finally:api.CloseHandle(handle)
            if exit_handle:
                try:assert kernel.WaitForSingleObject(exit_handle,15000)==0,'Owned fixture process did not exit'
                finally:kernel.CloseHandle(exit_handle)
            else:time.sleep(.5)
    print(json.dumps({'checks':checks,'model_calls':0,'production_operations':0,'binary_sha256':digest(binary),'condition':'real native binaries, same version; distinct ad-hoc signature identifier on macOS or inert trailing bytes elsewhere; inert Instance setup inventory never executed; bad candidate version injected after staging to test actual child rejection and rollback'},indent=2))
if __name__=='__main__':main()
