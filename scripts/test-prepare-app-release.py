"""Exact app-only publication set and embedded helper pin; inert binaries only."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec=importlib.util.spec_from_file_location('release',Path(__file__).with_name('prepare-app-release.py'))
release=importlib.util.module_from_spec(spec);spec.loader.exec_module(release)

class ReleaseSet(unittest.TestCase):
    def fixture(self, root):
        source=root/'artifacts';source.mkdir();binary=root/'inert';binary.write_bytes(b'inert app, never executed')
        rows=[];files={}
        for platform in release.helper_stage.PLATFORMS:
            stream=io.BytesIO()
            with zipfile.ZipFile(stream,'w') as z:z.writestr('helper',b'inert '+platform.encode())
            name=platform+'.zip';data=stream.getvalue();files[name]=data
            rows.append({'platform':platform,'filename':name,'sha256':hashlib.sha256(data).hexdigest()})
        helper=root/'helpers.zip'
        with zipfile.ZipFile(helper,'w') as z:
            for name,data in files.items():z.writestr(name,data)
            z.writestr('manifest.json',json.dumps({'schema':1,'artifacts':rows}))
        pin=release.digest(helper)
        for target in release.packaging.TARGETS:
            release.packaging.package(binary,target,'0.11.1',source)
            instance=root/target;instance.mkdir();name='agent-instance.exe' if 'windows' in target else 'agent-instance'
            (instance/name).write_bytes(b'inert instance, never executed')
            (instance/'consumer-bundle.json').write_text(json.dumps({'schema':1,'kind':'instance-consumer-bundle','target':target,'version':'fixture','source':'0'*40,'binary':name,'native_runtime':{'version':'fixture'},'files':{name:release.digest(instance/name)}}))
            release.packaging.package(binary,target,'0.11.1',source,instance,machine_helpers=helper,helpers_sha256=pin)
        (source/'constellation-machine-helper-0.2.0-linux-x86_64.zip').write_bytes(b'new rebuilt helper must not publish')
        return source,helper,pin
    def test_exact_38_assets_retain_pinned_helpers_and_exclude_rebuilds(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);source,helper,pin=self.fixture(root)
            with patch.object(release.helpers,'SHA256',pin):manifest=release.prepare(source,root/'out','0.11.1',helper)
            self.assertEqual(len(manifest),38)
            self.assertEqual(manifest[release.HELPER_NAME],pin)
            self.assertFalse(any(name.startswith('constellation-machine-helper-') for name in manifest))
            self.assertEqual((root/'out'/release.HELPER_NAME).read_bytes(),helper.read_bytes())
    def test_wrong_helper_pin_and_existing_output_are_preserved(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);source,helper,pin=self.fixture(root)
            with self.assertRaisesRegex(ValueError,'checksum'):release.prepare(source,root/'out','0.11.1',helper)
            output=root/'existing';output.mkdir();(output/'keep').write_text('original')
            with self.assertRaisesRegex(ValueError,'empty'):release.prepare(source,output,'0.11.1',helper)
            self.assertEqual((output/'keep').read_text(),'original')
    def test_metadata_and_archive_checksum_refusals(self):
        for mutation in ('metadata','archive'):
            with self.subTest(mutation=mutation),tempfile.TemporaryDirectory() as d:
                root=Path(d);source,helper,pin=self.fixture(root)
                file=source/'agent-observatory-0.11.1-x86_64-unknown-linux-gnu.tar.gz'
                if mutation=='metadata':
                    p=Path(str(file)+'.json');value=json.loads(p.read_text());value['target']='wrong';p.write_text(json.dumps(value))
                else:file.write_bytes(file.read_bytes()+b'changed')
                with patch.object(release.helpers,'SHA256',pin),self.assertRaisesRegex(ValueError,'differs'):release.prepare(source,root/'out','0.11.1',helper)
                self.assertEqual(list((root/'out').iterdir()),[])
    def test_rehashed_outer_archive_cannot_hide_changed_embedded_helper(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);source,helper,pin=self.fixture(root)
            file=source/'agent-observatory-0.11.1-x86_64-pc-windows-msvc-desktop.zip'
            with zipfile.ZipFile(file) as z:members={name:z.read(name) for name in z.namelist()}
            name=next(n for n in members if n.startswith('support/machine-helpers/') and n.endswith('.zip'))
            members[name]=b'changed helper'
            with zipfile.ZipFile(file,'w') as z:
                for name,data in members.items():z.writestr(name,data)
            p=Path(str(file)+'.json');value=json.loads(p.read_text());value['sha256']=release.digest(file);p.write_text(json.dumps(value));Path(str(file)+'.sha256').write_text(value['sha256']+'  '+file.name+'\n')
            with patch.object(release.helpers,'SHA256',pin),self.assertRaisesRegex(ValueError,'helper bytes'):release.prepare(source,root/'out','0.11.1',helper)
            self.assertEqual(list((root/'out').iterdir()),[])

if __name__=='__main__':unittest.main()
