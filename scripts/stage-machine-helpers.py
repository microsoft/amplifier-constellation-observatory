#!/usr/bin/env python3
"""Stage a verified release bundle; no pairing, service or agent operation."""
import argparse, hashlib, io, json, os, stat, tempfile, uuid, zipfile
from pathlib import Path
PLATFORMS={'windows-aarch64','windows-x86_64','macos-aarch64','macos-x86_64','linux-aarch64','linux-x86_64'}
def verified_files(bundle,expected):
    if bundle.stat().st_size>400*1024*1024:raise ValueError('Bundle is too large')
    if hashlib.sha256(bundle.read_bytes()).hexdigest()!=expected:raise ValueError('Bundle checksum does not match the reviewed release')
    with zipfile.ZipFile(bundle) as z:
        names=z.namelist()
        if len(names) not in (6,7) or len(set(names))!=len(names) or 'manifest.json' not in names:raise ValueError('Invalid release bundle members')
        if z.getinfo('manifest.json').file_size>65536:raise ValueError('Manifest is too large')
        manifest=json.loads(z.read('manifest.json'));artifacts=manifest['artifacts']
        actual_platforms={a['platform'] for a in artifacts}
        if manifest['schema']!=1 or actual_platforms not in (PLATFORMS, PLATFORMS-{'windows-aarch64'}) or len(artifacts)!=len(actual_platforms):raise ValueError('Expected a complete five- or six-platform release')
        if set(names)!={'manifest.json',*[a['filename']for a in artifacts]}:raise ValueError('Unexpected release payload')
        verified={}
        for a in artifacts:
            name=a['filename']
            if Path(name).name!=name or '/' in name or '\\' in name or not name.endswith('.zip'):raise ValueError('Invalid archive name')
            if z.getinfo(name).file_size>80*1024*1024:raise ValueError('Platform archive is too large')
            data=z.read(name)
            if hashlib.sha256(data).hexdigest()!=a['sha256']:raise ValueError('Platform checksum mismatch')
            try:
                with zipfile.ZipFile(io.BytesIO(data))as inner:
                    members=inner.infolist()
                    if not members or len(members)>50:raise ValueError('Invalid helper archive')
                    for member in members:
                        path=Path(member.filename)
                        if path.is_absolute() or '..'in path.parts or '\\'in member.filename or ':'in member.filename or path.name.casefold()=='observatory-pairing.json' or member.file_size>80*1024*1024 or stat.S_ISLNK(member.external_attr>>16):raise ValueError('Unsafe helper archive')
            except zipfile.BadZipFile as e:raise ValueError('Invalid helper ZIP')from e
            verified[name]=data
        verified['manifest.json']=z.read('manifest.json')
        return manifest,verified

def stage(home,bundle,expected):
    if not (home/'config.json').is_file():raise ValueError('Use an existing Observatory installation')
    manifest,verified=verified_files(bundle,expected)
    artifacts=manifest['artifacts'];actual_platforms={a['platform']for a in artifacts}
    with tempfile.TemporaryDirectory(prefix='.helper-stage-',dir=home) as temp:
        prepared=Path(temp)/'machine-helper-artifacts';prepared.mkdir(mode=0o700)
        for name,data in verified.items():(prepared/name).write_bytes(data)
        target=home/'machine-helper-artifacts';backup=None
        if target.exists():
            if target.is_symlink():raise ValueError('Refusing to replace a symbolic link')
            existing=json.loads((target/'manifest.json').read_text())
            if existing==manifest and all(hashlib.sha256((target/a['filename']).read_bytes()).hexdigest()==a['sha256']for a in artifacts):return {'unchanged':True,'platforms':sorted(actual_platforms)}
            backup=home/('machine-helper-artifacts-backup-'+str(uuid.uuid4()));os.replace(target,backup)
        try:os.replace(prepared,target)
        except BaseException:
            if backup is not None:os.replace(backup,target)
            raise
    return {'installed':str(target),'backup':str(backup)if backup else None,'platforms':sorted(actual_platforms),'pairings_created':0,'service_operations':0}
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--home',type=Path,required=True);p.add_argument('--bundle',type=Path,required=True);p.add_argument('--sha256',required=True);a=p.parse_args();print(json.dumps(stage(a.home,a.bundle,a.sha256),indent=2))
