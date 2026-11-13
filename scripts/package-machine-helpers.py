#!/usr/bin/env python3
"""Combine exact platform release archives for an Observatory's setup downloads."""
import argparse, hashlib, json, zipfile
from pathlib import Path

PLATFORMS={'windows-x86_64':('Windows','Intel / AMD'),'windows-aarch64':('Windows','ARM64'),'macos-aarch64':('Mac','Apple silicon'),'macos-x86_64':('Mac','Intel'),'linux-aarch64':('Linux','ARM64'),'linux-x86_64':('Linux','Intel / AMD')}
def package(directory,version='0.2.0'):
    artifacts=[]
    for platform,(label,architecture) in PLATFORMS.items():
        filename=f'constellation-machine-helper-{version}-{platform}.zip';path=directory/filename
        record=json.loads((directory/(filename+'.json')).read_text())
        digest=hashlib.sha256(path.read_bytes()).hexdigest()
        if record['sha256']!=digest or record['platform']!=platform or record['version']!=version:raise ValueError('Platform build provenance mismatch')
        if record.get('signed_or_notarized') is True:raise ValueError('Signed packages require a separately opened setup file; this packager embeds public setup in the unsigned Mac bundle')
        if (directory/(filename+'.sha256')).read_text().split()!=[digest,filename]:raise ValueError('Platform checksum mismatch')
        row=dict(platform=platform,label=label,architecture=architecture,filename=filename,sha256=digest,version=version)
        if record.get('installation_protocol') == 1:row['installation_protocol']=1
        artifacts.append(row)
    manifest={'schema':1,'artifacts':artifacts};output=directory/f'constellation-machine-helpers-{version}.zip'
    with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_STORED) as z:
        # Retrying a partial draft upload with the same builds must retain bytes.
        z.writestr(zipfile.ZipInfo('manifest.json'),json.dumps(manifest,indent=2)+'\n')
        for a in artifacts:z.writestr(zipfile.ZipInfo(a['filename']),(directory/a['filename']).read_bytes())
    digest=hashlib.sha256(output.read_bytes()).hexdigest();(directory/(output.name+'.sha256')).write_text(digest+'  '+output.name+'\n');return output
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--directory',type=Path,required=True);p.add_argument('--version',default='0.2.0');a=p.parse_args();print(package(a.directory,a.version))
