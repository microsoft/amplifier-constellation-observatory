#!/usr/bin/env python3
"""Package format and native import boundaries; synthetic executable bytes never run."""
import hashlib, importlib.util, io, json, plistlib, stat, struct, tarfile, tempfile, unittest, zipfile
from pathlib import Path, PurePosixPath

def module(name, filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename));loaded=importlib.util.module_from_spec(spec);spec.loader.exec_module(loaded);return loaded
pack=module('pack','package-observatory.py'); macho=module('macho','check-native-macho.py')
class PackagingTests(unittest.TestCase):
    def test_six_archives_have_native_entrypoint_hashes_and_repeatable_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);binary=root/'binary';binary.write_bytes(b'inert native fixture, never launched')
            for target in pack.TARGETS:
                with self.subTest(target=target):
                    row=pack.package(binary,target,'0.11.0',root/'dist');path=root/'dist'/row['filename'];original=path.read_bytes()
                    self.assertFalse(row['signed_or_notarized']);self.assertFalse(row['requires_developer_toolchain'])
                    self.assertEqual(row['binary_sha256'],hashlib.sha256(binary.read_bytes()).hexdigest())
                    if row['os']=='linux':
                        with tarfile.open(path) as z:
                            self.assertEqual(z.getnames(),['agent-observatory','observatory-package.json']);self.assertEqual(z.extractfile('agent-observatory').read(),binary.read_bytes());self.assertEqual(z.getmembers()[0].mode,0o755)
                            manifest=json.load(z.extractfile('observatory-package.json'))
                    else:
                        with zipfile.ZipFile(path) as z:
                            self.assertEqual(z.read(row['entry']),binary.read_bytes());self.assertTrue(z.getinfo(row['entry']).external_attr>>16 & stat.S_IXUSR)
                            self.assertTrue(all('\\' not in name for name in z.namelist()))
                            manifest=json.loads(z.read(str(PurePosixPath(row['entry']).parent/'observatory-package.json')))
                            if row['os']=='macos':
                                info=plistlib.loads(z.read('Observatory.app/Contents/Info.plist'));self.assertEqual(info['CFBundleExecutable'],'agent-observatory');self.assertEqual(info['CFBundlePackageType'],'APPL')
                    self.assertEqual(manifest['binary_sha256'],row['binary_sha256']);self.assertEqual(manifest['target'],target);self.assertEqual(manifest['version'],row['version']);self.assertNotIn('instance_manifest_sha256',manifest)
                    pack.package(binary,target,'0.11.0',root/'dist');self.assertEqual(path.read_bytes(),original)
                    self.assertEqual(json.loads(path.with_name(path.name+'.json').read_text()),row)
    def test_complete_package_keeps_matching_instance_closure_and_refuses_changed_member(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);binary=root/'observatory';binary.write_bytes(b'inert Observatory fixture')
            for target in pack.TARGETS:
                source=root/target;source.mkdir();name='agent-instance.exe' if 'windows' in target else 'agent-instance'
                (source/name).write_bytes(b'inert Instance fixture');(source/name).chmod(0o755)
                manifest={'schema':1,'kind':'instance-consumer-bundle','version':'fixture','source':'0'*40,'target':target,'binary':name,'native_runtime':{'version':'fixture'},'files':{name:hashlib.sha256((source/name).read_bytes()).hexdigest()}}
                (source/'consumer-bundle.json').write_text(json.dumps(manifest))
                record=pack.package(binary,target,'0.11.0',root/'out',source)
                self.assertTrue(record['includes_agent_setup']);self.assertIn('-desktop.',record['filename'])
                prefix='Observatory.app/Contents/MacOS/' if 'apple' in target else ''
                if record['os']=='linux':
                    with tarfile.open(root/'out'/record['filename']) as archive:
                        self.assertEqual(archive.extractfile('support/instance/'+name).read(),b'inert Instance fixture')
                        package_manifest=json.load(archive.extractfile('observatory-package.json'))
                else:
                    with zipfile.ZipFile(root/'out'/record['filename']) as archive:
                        self.assertEqual(archive.read(prefix+'support/instance/'+name),b'inert Instance fixture')
                        package_manifest=json.loads(archive.read(prefix+'observatory-package.json'))
                self.assertEqual(package_manifest['instance_manifest_sha256'],hashlib.sha256((source/'consumer-bundle.json').read_bytes()).hexdigest())
                (source/name).write_bytes(b'changed')
                with self.assertRaisesRegex(ValueError,'integrity'):pack.package(binary,target,'0.11.0',root/'out',source)
                manifest['target']='wrong';(source/'consumer-bundle.json').write_text(json.dumps(manifest))
                with self.assertRaisesRegex(ValueError,'matching'):pack.package(binary,target,'0.11.0',root/'out',source)
    def test_native_package_retains_all_verified_helper_downloads_and_rejects_tampering(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); binary=root/'binary';binary.write_bytes(b'inert app fixture')
            helper_pack=module('helpers','package-machine-helpers.py');folder=root/'helpers';folder.mkdir()
            for platform in helper_pack.PLATFORMS:
                name=f'constellation-machine-helper-0.2.0-{platform}.zip'
                stream=io.BytesIO()
                with zipfile.ZipFile(stream,'w') as archive:archive.writestr('helper',b'inert helper')
                data=stream.getvalue();(folder/name).write_bytes(data)
                (folder/(name+'.json')).write_text(json.dumps({'platform':platform,'version':'0.2.0','sha256':hashlib.sha256(data).hexdigest()}))
                (folder/(name+'.sha256')).write_text(hashlib.sha256(data).hexdigest()+'  '+name+'\n')
            bundle=helper_pack.package(folder);expected=hashlib.sha256(bundle.read_bytes()).hexdigest()
            with zipfile.ZipFile(bundle) as archive: members={name:archive.read(name) for name in archive.namelist()}
            for target in pack.TARGETS:
                record=pack.package(binary,target,'0.11.0',root/'out',machine_helpers=bundle,helpers_sha256=expected)
                self.assertTrue(record['includes_computer_setup'])
                path=root/'out'/record['filename'];before=path.read_bytes()
                prefix='Observatory.app/Contents/MacOS/' if 'apple' in target else ''
                if record['os']=='linux':
                    with tarfile.open(path) as archive:
                        saved={name:archive.extractfile('support/machine-helpers/'+name).read() for name in members}
                        manifest=json.load(archive.extractfile('observatory-package.json'))
                else:
                    with zipfile.ZipFile(path) as archive:
                        saved={name:archive.read(prefix+'support/machine-helpers/'+name) for name in members}
                        manifest=json.loads(archive.read(prefix+'observatory-package.json'))
                self.assertEqual(saved,members)
                self.assertEqual(manifest['helpers_manifest_sha256'],hashlib.sha256(members['manifest.json']).hexdigest())
                with self.assertRaisesRegex(ValueError,'checksum'):
                    pack.package(binary,target,'0.11.0',root/'out',machine_helpers=bundle,helpers_sha256='0'*64)
                self.assertEqual(path.read_bytes(),before)
    def fixture(self,cpu,library):
        name=library.encode()+b'\0';size=(24+len(name)+7)//8*8
        command=struct.pack('<6I',0xc,size,24,0,0,0)+name+bytes(size-24-len(name))
        return struct.pack('<8I',0xfeedfacf,cpu,0,2,1,len(command),0,0)+command
    def test_macos_system_imports_only_and_architecture_match(self):
        for architecture,cpu in [('x86_64',0x01000007),('aarch64',0x0100000c)]:
            good=self.fixture(cpu,'/usr/lib/libSystem.B.dylib');self.assertTrue(macho.inspect(good,architecture)['system_libraries_only'])
            for bad in ['@rpath/libcustom.dylib','/opt/homebrew/lib/libssl.dylib']:
                self.assertFalse(macho.inspect(self.fixture(cpu,bad),architecture)['system_libraries_only'])
            with self.assertRaises(ValueError):macho.inspect(good,'aarch64' if architecture=='x86_64' else 'x86_64')
            with self.assertRaises(ValueError):macho.inspect(good[:-3],architecture)
if __name__=='__main__':unittest.main(verbosity=2)
