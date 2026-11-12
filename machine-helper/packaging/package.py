#!/usr/bin/env python3
"""Package a platform-built helper; bootstrap is appended by its Observatory."""
import argparse, hashlib, json, os, plistlib, stat, zipfile
from pathlib import Path

PLATFORMS={'windows-aarch64','windows-x86_64','macos-aarch64','macos-x86_64','linux-aarch64','linux-x86_64'}
def package(binary,platform,version,output):
    if platform not in PLATFORMS:raise ValueError('Unsupported platform')
    if not binary.is_file() or binary.is_symlink():raise ValueError('Supply the compiled ordinary executable')
    output.mkdir(parents=True,exist_ok=True)
    filename=f'constellation-machine-helper-{version}-{platform}.zip'
    archive=output/filename
    executable='constellation-machine-helper.exe' if platform.startswith('windows') else 'constellation-machine-helper'
    prefix='Constellation Computer.app/Contents/' if platform.startswith('macos') else ''
    def put(z,name,data,mode=0o644):
        info=zipfile.ZipInfo(name);info.create_system=3;info.external_attr=(stat.S_IFREG|mode)<<16;info.compress_type=zipfile.ZIP_DEFLATED;z.writestr(info,data)
    with zipfile.ZipFile(archive,'w') as z:
        put(z,prefix+('MacOS/' if prefix else '')+executable,binary.read_bytes(),0o755)
        if prefix:
            put(z,prefix+'Info.plist',plistlib.dumps({'CFBundleName':'Constellation Computer','CFBundleDisplayName':'Constellation Computer','CFBundleIdentifier':'com.microsoft.constellation.machine-helper','CFBundleExecutable':executable,'CFBundleVersion':version,'CFBundleShortVersionString':version,'CFBundlePackageType':'APPL','LSMinimumSystemVersion':'11.0','LSUIElement':True}))
        put(z,'START-HERE.txt',b'Open Constellation Computer.app on Mac, or constellation-machine-helper on Windows/Linux.\nUse the setup download from your Observatory; it supplies observatory-pairing.json.\nThe base package alone has no pairing destination, code or credential.\n')
        root=Path(__file__).resolve().parents[2]
        put(z,'LICENSE', (root/'LICENSE').read_bytes())
        put(z,'NUNITO-LICENSE.txt',(Path(__file__).resolve().parents[1]/'ui/NUNITO-LICENSE.txt').read_bytes())
    digest=hashlib.sha256(archive.read_bytes()).hexdigest()
    (output/(filename+'.sha256')).write_text(digest+'  '+filename+'\n')
    record={'platform':platform,'version':version,'filename':filename,'sha256':digest,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'bootstrap_path':'observatory-pairing.json','bootstrap_included':False,'signed_or_notarized':False}
    (output/(filename+'.json')).write_text(json.dumps(record,indent=2)+'\n')
    return record
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--binary',required=True,type=Path);p.add_argument('--platform',required=True,choices=sorted(PLATFORMS));p.add_argument('--version',required=True);p.add_argument('--output',type=Path,default=Path('dist'));a=p.parse_args();print(json.dumps(package(a.binary,a.platform,a.version,a.output),indent=2))
