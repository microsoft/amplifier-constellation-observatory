"""Inert fixed-asset intake fixtures; no network, signing or product execution."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import Mock, patch
import uuid
import zipfile

spec = importlib.util.spec_from_file_location('intake', Path(__file__).with_name('fetch-signed-build-inputs.py'))
intake = importlib.util.module_from_spec(spec); spec.loader.exec_module(intake)
sha = lambda value: hashlib.sha256(value).hexdigest()
raw = lambda value: (json.dumps(value, sort_keys=True) + '\n').encode()


def archive(files):
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as output:
        for name, value in sorted(files.items()):
            entry = zipfile.ZipInfo(name); entry.create_system = 3
            entry.external_attr = (stat.S_IFREG | 0o644) << 16
            output.writestr(entry, value)
    return data.getvalue()


class Transport:
    def __init__(self, fixture):
        self.bytes = fixture.assets
        self.metadata = {'id':123, 'draft':True, 'assets':[
            {'id':r['id'],'name':r['name'],'size':r['size'],'state':'uploaded','digest':'sha256:'+r['sha256']}
            for r in fixture.rows]}
        self.reads = []; self.downloads = []
    def release(self, release_id):
        self.reads.append(release_id); return copy.deepcopy(self.metadata)
    def download(self, row, path):
        self.downloads.append(row['id']); path.write_bytes(self.bytes[row['id']])


class Fixture:
    def __init__(self, root):
        self.root = root; self.assets = {}; self.rows = []
        self.instance = {'schema':1,'targets':{}}; self.slack = {'schema':1,'targets':{}}
        self.cohort = {'release_id':123,'instance':{},'slack':{}}
        for index, target in enumerate(sorted(intake.MAC)):
            self.make_instance(target, index); self.make_slack(target)
        helpers = []; nested = {}
        for platform in sorted(intake.module('stage-machine-helpers').PLATFORMS):
            files = {'helper':b'inert helper bytes; never executed'}
            row = {'platform':platform,'source':'b'*40,'version':'0.3.5','filename':platform+'.zip'}
            if platform.startswith('macos-'):
                files={'Constellation Computer.app/Contents/MacOS/constellation-machine-helper':b'inert helper',
                       'Constellation Computer.app/Contents/Info.plist':b'plist',
                       'Constellation Computer.app/Contents/_CodeSignature/CodeResources':b'inert seal',
                       'Constellation Computer.app/Contents/CodeResources':b'inert staple ticket',
                       'START-HERE.txt':b'notice','LICENSE':b'license','NUNITO-LICENSE.txt':b'font license'}
                row.update(bootstrap_delivery='separate_file', signed_derivative={'schema':1,
                    'origin_archive_sha256':'2'*64,'origin_binary_sha256':'3'*64,
                    'signing_receipt_sha256':'4'*64,'team_id':'ABCDEFGHIJ'})
            nested[row['filename']] = archive(files); row['sha256'] = sha(nested[row['filename']]); helpers.append(row)
        manifest = raw({'schema':1,'source':'b'*40,'artifacts':helpers})
        nested['manifest.json'] = manifest
        self.cohort['helpers'] = {'asset':self.add('helpers.zip',archive(nested)),
                                 'source':'b'*40,'version':'0.3.5','manifest_sha256':sha(manifest)}
        self.lock = {'schema':1,'repository':intake.REPOSITORY,'cohort':self.cohort}
        self.save()

    def add(self, name, data):
        row = {'id':len(self.rows)+1,'name':name,'size':len(data),'sha256':sha(data)}
        self.rows.append(row); self.assets[row['id']] = data; return row

    def make_instance(self, target, index):
        files = {'agent-instance':b'inert signed Instance', 'engine/native/agent-runtime':b'inert signed Runtime',
                 'setup/native-runtime-lock.json':b'unchanged public Runtime lock',
                 'setup/private-python-lock.json':b'unchanged public Python lock'}
        pin = {'schema':1,'cohort_id':str(uuid.UUID(int=index+1)),'target':target,
            'instance':{'source':'1'*40,'tree':'2'*40,'version':'0.22.14','archive_sha256':'1'*64,'manifest_sha256':'2'*64},
            'runtime':{'source':'3'*40,'version':'0.15.12','archive_sha256':'3'*64,'binary_sha256':'4'*64,
                       'lock_sha256':sha(files['setup/native-runtime-lock.json'])},
            'python':{'archive_sha256':'5'*64,'lock_sha256':sha(files['setup/private-python-lock.json'])},
            'manifest_sha256':'0'*64,'instance_binary_sha256':sha(files['agent-instance']),
            'runtime_binary_sha256':sha(files['engine/native/agent-runtime']),
            'signing_receipt_sha256':'6'*64,'signing_team_id':'ABCDEFGHIJ'}
        manifest = {'schema':1,'kind':'instance-consumer-bundle','source':pin['instance']['source'],
            'version':'0.22.14','target':target,'binary':'agent-instance','python_archive_sha256':'5'*64,
            'private_derivative':{'schema':1,'kind':'signed_macos_instance','cohort_id':pin['cohort_id']},
            'native_runtime':{'kind':'private_derivative','binary':'engine/native/agent-runtime','version':'0.15.12',
                'platform':'macos-'+target.split('-')[0],'binary_sha256':pin['runtime_binary_sha256'],
                'public_origin':{'source':'3'*40,'archive_sha256':'3'*64,'binary_sha256':'4'*64}},
            'files':{name:sha(data) for name,data in files.items()}}
        files['consumer-bundle.json'] = raw(manifest); pin['manifest_sha256'] = sha(files['consumer-bundle.json'])
        self.instance['targets'][target] = pin
        self.cohort['instance'][target] = self.add('instance-'+target+'.zip',archive(files))

    def make_slack(self, target):
        prefix = 'Slack Connector.app/Contents/'
        entry = prefix+'MacOS/acp-slack-adapter'; native = prefix+'Resources/native-bundle.json'
        files = {entry:b'inert signed Slack',native:raw({'schema':1,'component':'acp-slack-adapter','version':'0.18.0','target':target}),
                 prefix+'_CodeSignature/CodeResources':b'inert seal',prefix+'Resources/LICENSE':b'notice',prefix+'Info.plist':b'plist'}
        data = archive(files); row = self.add('slack-'+target+'.zip',data)
        release = {'schema':1,'source':'a'*40,'version':'0.18.0','target':target,'entry':entry,
            'archive_size':len(data),'archive_sha256':sha(data),'native_bundle_sha256':sha(files[native]),
            'files':{name:{'size':len(value),'sha256':sha(value),'mode':0o644} for name,value in files.items()},
            'signed_derivative':{'schema':1,'origin_archive_sha256':'7'*64,'origin_native_bundle_sha256':'8'*64,
                                 'signing_receipt_sha256':'9'*64,'team_id':'ABCDEFGHIJ'}}
        self.slack['targets'][target] = {'url':None,'release':release}; self.cohort['slack'][target] = row

    def save(self):
        for name,value in [('inputs',self.lock),('instance',self.instance),('slack',self.slack)]:
            (self.root/(name+'.json')).write_bytes(raw(value))
        self.args = {'lock_path':self.root/'inputs.json','instance_lock':self.root/'instance.json','slack_lock':self.root/'slack.json'}


class IntakeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name); self.f = Fixture(self.root)
        self.target = sorted(intake.MAC)[0]

    def test_exact_inert_mac_inputs_and_all_six_helpers_publish_once(self):
        transport = Transport(self.f); out = self.root/'target'
        result = intake.fetch_target(self.target,out,transport=transport,**self.f.args)
        self.assertEqual(result['source_kind'],'retained_signed_owner_cohort')
        self.assertEqual((out/'instance/agent-instance').read_bytes(),b'inert signed Instance')
        self.assertEqual((out/'slack.zip').read_bytes(),self.f.assets[self.f.cohort['slack'][self.target]['id']])
        helper = self.root/'helpers.zip'; intake.fetch_helpers(helper,transport=transport,**self.f.args)
        self.assertEqual(helper.read_bytes(),self.f.assets[self.f.cohort['helpers']['asset']['id']])
        self.assertEqual(len(transport.downloads),3)
        before = list(transport.downloads)
        with self.assertRaisesRegex(ValueError,'preserved'):intake.fetch_target(self.target,out,transport=transport,**self.f.args)
        self.assertEqual(before,transport.downloads)

    def test_unarmed_has_no_transport_or_output_and_nonmac_never_uses_private_path(self):
        self.f.lock['cohort'] = None; self.f.save(); transport = Transport(self.f)
        self.assertIsNone(intake.selection(**self.f.args))
        for target in [self.target,'x86_64-pc-windows-msvc']:
            with self.assertRaises(ValueError):intake.fetch_target(target,self.root/'out',transport=transport,**self.f.args)
        self.assertEqual(transport.reads,[]); self.assertEqual(transport.downloads,[]); self.assertFalse((self.root/'out').exists())

    def test_malformed_owner_asset_schema_or_missing_target_refuses_before_transport(self):
        original = copy.deepcopy(self.f.lock)
        changes = [lambda v:v.update(repository='other/repository'),lambda v:v.update(schema=True),
                   lambda v:v['cohort']['helpers']['asset'].update(url='https://example.invalid/file.zip'),
                   lambda v:v['cohort']['helpers']['asset'].update(id=True),
                   lambda v:v['cohort']['helpers']['asset'].update(name='../escape.zip'),
                   lambda v:v['cohort']['instance'].pop(self.target)]
        for change in changes:
            self.f.lock=copy.deepcopy(original); change(self.f.lock); self.f.save(); transport=Transport(self.f)
            with self.assertRaises(ValueError):intake.fetch_helpers(self.root/'out',transport=transport,**self.f.args)
            self.assertEqual(transport.reads,[]); self.assertFalse((self.root/'out').exists())

    def test_compiled_slack_schema_and_origin_hash_types_are_not_coerced(self):
        original=copy.deepcopy(self.f.slack)
        for change in [lambda v:v.update(schema=True),
                       lambda v:v['signed_derivative'].update(origin_archive_sha256=int('1'*64))]:
            self.f.slack=copy.deepcopy(original);change(self.f.slack['targets'][self.target]['release'])
            self.f.save();transport=Transport(self.f)
            with self.assertRaises(ValueError):intake.fetch_helpers(self.root/'out',transport=transport,**self.f.args)
            self.assertEqual(transport.reads,[]);self.assertEqual(transport.downloads,[])

    def test_draft_membership_digest_and_state_must_match_before_download(self):
        for change in [lambda v:v.update(draft=False),lambda v:v.update(id=99),
                       lambda v:v['assets'][0].update(digest='sha256:'+'0'*64),
                       lambda v:v['assets'][0].update(state='new')]:
            transport=Transport(self.f); change(transport.metadata)
            with self.assertRaises(ValueError):intake.fetch_helpers(self.root/'out',transport=transport,**self.f.args)
            self.assertEqual(transport.downloads,[]); self.assertFalse((self.root/'out').exists())

    def test_truncated_changed_or_oversized_asset_never_publishes_or_falls_back(self):
        row=self.f.cohort['helpers']['asset']; original=self.f.assets[row['id']]
        for data in [original[:-1],b'X'+original[1:],original+b'extra']:
            transport=Transport(self.f); transport.bytes=dict(self.f.assets); transport.bytes[row['id']]=data
            with self.assertRaises(ValueError):intake.fetch_helpers(self.root/'out',transport=transport,**self.f.args)
            self.assertEqual(transport.downloads,[row['id']]); self.assertFalse((self.root/'out').exists())

    def test_manifest_cohort_hash_and_origin_are_bound_to_compiled_descriptor(self):
        pin=self.f.instance['targets'][self.target]; original=copy.deepcopy(pin)
        for field,value in [('manifest_sha256','0'*64),('cohort_id',str(uuid.uuid4())),('instance',{**pin['instance'],'source':'f'*40})]:
            self.f.instance['targets'][self.target]=copy.deepcopy(original)
            self.f.instance['targets'][self.target][field]=value; self.f.save()
            with self.assertRaises(ValueError):intake.fetch_target(self.target,self.root/'out',transport=Transport(self.f),**self.f.args)
            self.assertFalse((self.root/'out').exists())

    def test_signed_slack_member_inventory_is_checked_not_just_outer_hash(self):
        release=self.f.slack['targets'][self.target]['release']
        release['files'][release['entry']]['sha256']='0'*64; self.f.save()
        with self.assertRaisesRegex(ValueError,'member differs'):
            intake.fetch_target(self.target,self.root/'out',transport=Transport(self.f),**self.f.args)
        self.assertFalse((self.root/'out').exists())

    def test_retained_helper_manifest_source_is_not_relabelled_same_run(self):
        self.f.cohort['helpers']['source']='e'*40; self.f.save()
        with self.assertRaisesRegex(ValueError,'closure identity'):
            intake.fetch_helpers(self.root/'out',transport=Transport(self.f),**self.f.args)
        self.assertFalse((self.root/'out').exists())

    def test_unlisted_instance_file_refuses_even_with_new_outer_asset_hash(self):
        row=self.f.cohort['instance'][self.target]
        with zipfile.ZipFile(io.BytesIO(self.f.assets[row['id']])) as z:files={n:z.read(n)for n in z.namelist()}
        files['extra-unlisted']=b'extra';data=archive(files);self.f.assets[row['id']]=data
        row.update(size=len(data),sha256=sha(data));self.f.save()
        with self.assertRaisesRegex(ValueError,'unlisted member'):
            intake.fetch_target(self.target,self.root/'out',transport=Transport(self.f),**self.f.args)
        self.assertFalse((self.root/'out').exists())

    def source_helpers(self):
        spec=importlib.util.spec_from_file_location('inert_helpers',intake.SCRIPTS.parent/'machine-helper/packaging/package.py')
        package=importlib.util.module_from_spec(spec);spec.loader.exec_module(package)
        binary=self.root/'inert-helper';binary.write_bytes(b'inert; never executed')
        source=self.root/'source-inputs'
        for platform in intake.module('stage-machine-helpers').PLATFORMS:
            package.package(binary,platform,'0.3.5',source,'c'*40)
        return source

    def test_empty_lock_keeps_same_run_helper_preparation_and_exact_bytes(self):
        source=self.source_helpers();self.f.lock['cohort']=None;self.f.save();transport=Transport(self.f)
        result=intake.select_helpers(source,self.root/'selected','c'*40,'0.3.5',transport=transport,**self.f.args)
        intake.module('prepare-helper-release').prepare(source,self.root/'baseline','0.3.5','c'*40)
        self.assertEqual(result['source_kind'],'same_run_source_inputs')
        for path in (self.root/'baseline').iterdir():
            self.assertEqual((self.root/'selected'/path.name).read_bytes(),path.read_bytes())
        self.assertEqual(transport.reads,[]);self.assertEqual(transport.downloads,[])

    def test_helper_only_bypasses_private_selection_but_app_selection_is_fail_closed(self):
        source=self.source_helpers();self.f.lock['repository']='wrong/owner';self.f.save();transport=Transport(self.f)
        with self.assertRaises(ValueError):
            intake.select_helpers(source,self.root/'app','c'*40,'0.3.5',transport=transport,**self.f.args)
        result=intake.select_helpers(source,self.root/'helper-only','c'*40,'0.3.5',source_inputs_only=True,transport=transport,**self.f.args)
        self.assertEqual(result['source_kind'],'same_run_source_inputs');self.assertEqual(transport.reads,[])

    def test_armed_helper_selection_keeps_retained_source_and_never_rebuilds(self):
        transport=Transport(self.f)
        result=intake.select_helpers(self.root/'no-source-inputs',self.root/'selected','c'*40,'0.3.5',transport=transport,**self.f.args)
        self.assertEqual(result['source_kind'],'retained_signed_owner_cohort')
        self.assertEqual(result['source'],'b'*40)
        self.assertEqual(result['sha256'],self.f.cohort['helpers']['asset']['sha256'])
        self.assertEqual(transport.downloads,[self.f.cohort['helpers']['asset']['id']])
        self.assertEqual(json.loads((self.root/'selected/signed-input-provenance.json').read_text()),result)

    def test_real_transport_shape_is_fixed_repo_bounded_and_never_logs_credentials(self):
        process=Mock();process.stdout=io.BytesIO(b'exact');process.wait.return_value=0;process.poll.return_value=0
        context=Mock();context.__enter__=Mock(return_value=process);context.__exit__=Mock(return_value=False)
        with patch.object(intake.subprocess,'Popen',return_value=context) as launch:
            intake.GitHubAssets.transfer('releases/assets/123',self.root/'transport',5,True)
        command=launch.call_args.args[0]
        self.assertEqual(command,['gh','api','--hostname','github.com','-H','Accept: application/octet-stream',
                                 'repos/microsoft/amplifier-constellation-observatory/releases/assets/123'])
        self.assertEqual(launch.call_args.kwargs['stderr'],intake.subprocess.DEVNULL)
        self.assertEqual((self.root/'transport').read_bytes(),b'exact')


if __name__ == '__main__': unittest.main()
