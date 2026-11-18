#!/usr/bin/env python3
"""Combine exact platform release archives for an Observatory's setup downloads."""
import argparse, hashlib, json, re, zipfile
from pathlib import Path

PLATFORMS={'windows-x86_64':('Windows','Intel / AMD'),'windows-aarch64':('Windows','ARM64'),'macos-aarch64':('Mac','Apple silicon'),'macos-x86_64':('Mac','Intel'),'linux-aarch64':('Linux','ARM64'),'linux-x86_64':('Linux','Intel / AMD')}

def distribution(record, platform):
    """Validate owner provenance; never infer or perform signing/notarization."""
    extra={}
    if 'bootstrap_delivery' in record:
        if not platform.startswith('macos-') or record['bootstrap_delivery']!='separate_file':
            raise ValueError('Unsupported helper bootstrap delivery')
        extra['bootstrap_delivery']='separate_file'
    signed=record.get('signed_or_notarized',False)
    if type(signed) is not bool:raise ValueError('Invalid helper distribution metadata')
    derivative=record.get('signed_derivative')
    if signed or 'signed_derivative' in record:
        fields={'schema','origin_archive_sha256','origin_binary_sha256','signing_receipt_sha256','team_id'}
        if (not signed or extra.get('bootstrap_delivery')!='separate_file'
                or not isinstance(derivative,dict) or set(derivative)!=fields
                or type(derivative['schema']) is not int or derivative['schema']!=1
                or any(not isinstance(derivative[k],str) or not re.fullmatch(r'[a-f0-9]{64}',derivative[k]) for k in fields-{'schema','team_id'})
                or not isinstance(derivative['team_id'],str) or not re.fullmatch(r'[A-Z0-9]{10}',derivative['team_id'])):
            raise ValueError('Signed helper requires exact owner provenance and separate setup file delivery')
        extra['signed_derivative']=derivative
    return extra

def package(directory,version='0.2.0'):
    artifacts=[];sources=[]
    for platform,(label,architecture) in PLATFORMS.items():
        filename=f'constellation-machine-helper-{version}-{platform}.zip';path=directory/filename
        record=json.loads((directory/(filename+'.json')).read_text())
        digest=hashlib.sha256(path.read_bytes()).hexdigest()
        if record['sha256']!=digest or record['platform']!=platform or record['version']!=version:raise ValueError('Platform build provenance mismatch')
        extra=distribution(record,platform)
        if extra.get('signed_derivative'):
            with zipfile.ZipFile(path) as archive:
                if 'Constellation Computer.app/Contents/_CodeSignature/CodeResources' not in archive.namelist():
                    raise ValueError('Signed helper resource seal is missing')
        if (directory/(filename+'.sha256')).read_text().split()!=[digest,filename]:raise ValueError('Platform checksum mismatch')
        sources.append(record.get('source'))
        row=dict(platform=platform,label=label,architecture=architecture,filename=filename,sha256=digest,version=version)
        row.update(extra)
        if record.get('installation_protocol') == 1:row['installation_protocol']=1
        if record.get('source') is not None:row['source']=record['source']
        artifacts.append(row)
    if any(sources) and (len(set(sources))!=1 or not re.fullmatch(r'[0-9a-f]{40}',sources[0] or '')):raise ValueError('Platform source revisions differ')
    manifest={'schema':1,'artifacts':artifacts}
    if any(sources):manifest['source']=sources[0]
    output=directory/f'constellation-machine-helpers-{version}.zip'
    with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_STORED) as z:
        # Retrying a partial draft upload with the same builds must retain bytes.
        z.writestr(zipfile.ZipInfo('manifest.json'),json.dumps(manifest,indent=2)+'\n')
        for a in artifacts:z.writestr(zipfile.ZipInfo(a['filename']),(directory/a['filename']).read_bytes())
    digest=hashlib.sha256(output.read_bytes()).hexdigest();(directory/(output.name+'.sha256')).write_text(digest+'  '+output.name+'\n');return output
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--directory',type=Path,required=True);p.add_argument('--version',default='0.2.0');a=p.parse_args();print(package(a.directory,a.version))
