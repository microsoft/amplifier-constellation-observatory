#!/usr/bin/env python3
"""CI-only intake of explicitly reviewed signed owner assets; never consumer setup."""
import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import threading
import uuid
import zipfile

REPOSITORY = 'microsoft/amplifier-constellation-observatory'
MAC = {'aarch64-apple-darwin', 'x86_64-apple-darwin'}
MAX_BYTES = 1024**3
SCRIPTS = Path(__file__).resolve().parent


def module(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), SCRIPTS / (name + '.py'))
    value = importlib.util.module_from_spec(spec); spec.loader.exec_module(value)
    return value


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def hex_value(value, count=64):
    return isinstance(value, str) and re.fullmatch(r'[a-f0-9]{%d}' % count, value) is not None


def version(value):
    return isinstance(value, str) and re.fullmatch(r'\d+\.\d+\.\d+', value) is not None


def load(path):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 8*1024**2,
            'Use the fixed ordinary build input lock')
    return json.loads(path.read_text(encoding='utf-8'))


def asset(row):
    require(isinstance(row, dict) and set(row) == {'id', 'name', 'size', 'sha256'}
            and type(row['id']) is int and row['id'] > 0
            and isinstance(row['name'], str) and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,200}\.zip', row['name'])
            and type(row['size']) is int and 0 < row['size'] <= MAX_BYTES
            and hex_value(row['sha256']), 'Invalid fixed signed asset')
    return row


def descriptor(target, value):
    fields = {'schema', 'cohort_id', 'target', 'instance', 'runtime', 'python',
              'manifest_sha256', 'instance_binary_sha256', 'runtime_binary_sha256',
              'signing_receipt_sha256', 'signing_team_id'}
    require(isinstance(value, dict) and set(value) == fields and type(value['schema']) is int
            and value['schema'] == 1 and value['target'] == target and target in MAC,
            'Invalid compiled signed Instance descriptor')
    require(isinstance(value['cohort_id'], str) and str(uuid.UUID(value['cohort_id'])) == value['cohort_id'],
            'Invalid signed Instance cohort')
    require(set(value['instance']) == {'source', 'tree', 'version', 'archive_sha256', 'manifest_sha256'}
            and set(value['runtime']) == {'source', 'version', 'archive_sha256', 'binary_sha256', 'lock_sha256'}
            and set(value['python']) == {'archive_sha256', 'lock_sha256'}, 'Invalid signed origins')
    for section in ('instance', 'runtime', 'python'):
        for key, item in value[section].items():
            require(version(item) if key == 'version' else hex_value(item, 40 if key in ('source', 'tree') else 64),
                    'Invalid signed origin value')
    require(all(hex_value(value[key]) for key in ('manifest_sha256', 'instance_binary_sha256',
                'runtime_binary_sha256', 'signing_receipt_sha256'))
            and isinstance(value['signing_team_id'], str)
            and re.fullmatch(r'[A-Z0-9]{10}', value['signing_team_id']), 'Invalid signed owner binding')
    return value


def selection(lock_path=None, instance_lock=None, slack_lock=None):
    value = load(lock_path or SCRIPTS / 'signed-build-inputs-lock.json')
    require(set(value) == {'schema', 'repository', 'cohort'} and type(value['schema']) is int
            and value['schema'] == 1 and value['repository'] == REPOSITORY, 'Invalid build input owner')
    cohort = value['cohort']
    if cohort is None:
        return None
    require(isinstance(cohort, dict) and set(cohort) == {'release_id', 'instance', 'slack', 'helpers'}
            and type(cohort['release_id']) is int and cohort['release_id'] > 0
            and isinstance(cohort['instance'], dict) and set(cohort['instance']) == MAC
            and isinstance(cohort['slack'], dict) and set(cohort['slack']) == MAC,
            'The signed cohort must close both Mac targets')
    helper = cohort['helpers']
    require(isinstance(helper, dict) and set(helper) == {'asset', 'source', 'version', 'manifest_sha256'}
            and hex_value(helper['source'], 40) and version(helper['version'])
            and hex_value(helper['manifest_sha256']), 'Invalid retained helper identity')
    rows = [asset(helper['asset'])]
    compiled = load(instance_lock or SCRIPTS / 'signed-instance-cohort-lock.json')
    require(set(compiled) == {'schema', 'targets'} and type(compiled['schema']) is int
            and compiled['schema'] == 1 and set(compiled['targets']) == MAC,
            'Both signed Instance descriptors must be compiled')
    slack_path = slack_lock or SCRIPTS / 'slack-bundle-lock.json'
    for target in sorted(MAC):
        rows += [asset(cohort['instance'][target]), asset(cohort['slack'][target])]
        descriptor(target, compiled['targets'][target])
        selected = module('fetch-slack-bundle').selected(target, slack_path)
        signed = selected['release'].get('signed_derivative')
        require(type(selected['release'].get('schema')) is int and selected['release']['schema'] == 1
                and isinstance(signed, dict)
                and all(hex_value(signed.get(key)) for key in ('origin_archive_sha256',
                        'origin_native_bundle_sha256', 'signing_receipt_sha256')),
                'Invalid typed signed Slack descriptor')
        require(selected['url'] is None and signed is not None
                and selected['release']['archive_sha256'] == cohort['slack'][target]['sha256']
                and selected['release']['archive_size'] == cohort['slack'][target]['size'],
                'Signed Slack asset differs from its compiled owner lock')
    require(len({r['id'] for r in rows}) == 5 and len({r['name'] for r in rows}) == 5,
            'Signed cohort assets must be distinct')
    return cohort


class GitHubAssets:
    """Fixed same-repository API only; gh credentials stay in its environment."""
    @staticmethod
    def transfer(route, output, limit, binary):
        command = ['gh', 'api', '--hostname', 'github.com']
        if binary:
            command += ['-H', 'Accept: application/octet-stream']
        command.append(f'repos/{REPOSITORY}/{route}')
        environment = os.environ.copy(); environment['GH_PROMPT_DISABLED'] = '1'
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              env=environment) as process:
            expired = threading.Event()
            def timeout():
                expired.set()
                try: process.kill()
                except ProcessLookupError: pass
            timer = threading.Timer(300, timeout); timer.start()
            try:
                total = 0
                with output.open('xb') as stream:
                    while block := process.stdout.read(1024*1024):
                        total += len(block)
                        require(total <= limit, 'Signed asset exceeded its fixed bound')
                        stream.write(block)
                    stream.flush(); os.fsync(stream.fileno())
                require(process.wait(timeout=5) == 0 and not expired.is_set(), 'Fixed GitHub asset transfer failed')
            finally:
                timer.cancel()
                if process.poll() is None: process.kill()
                process.wait(timeout=5)

    def release(self, release_id):
        with tempfile.TemporaryDirectory(prefix='signed-release-metadata-') as temporary:
            output = Path(temporary) / 'metadata.json'
            self.transfer(f'releases/{release_id}', output, 8*1024**2, False)
            return load(output)

    def download(self, row, output):
        self.transfer(f'releases/assets/{row["id"]}', output, row['size'], True)


def verify_release(cohort, transport):
    release = transport.release(cohort['release_id'])
    require(isinstance(release, dict) and release.get('id') == cohort['release_id']
            and release.get('draft') is True and isinstance(release.get('assets'), list),
            'Use the exact reviewed draft release')
    rows = [cohort['helpers']['asset'], *cohort['instance'].values(), *cohort['slack'].values()]
    for row in rows:
        matched = [r for r in release['assets'] if r.get('id') == row['id']]
        require(len(matched) == 1 and matched[0].get('name') == row['name']
                and matched[0].get('size') == row['size'] and matched[0].get('state') == 'uploaded'
                and matched[0].get('digest') == 'sha256:' + row['sha256'],
                'Draft asset membership or digest changed')


def download(row, destination, transport):
    transport.download(row, destination)
    require(destination.is_file() and not destination.is_symlink()
            and destination.stat().st_size == row['size'], 'Signed asset size differs')
    with destination.open('rb') as stream:
        require(hashlib.file_digest(stream, 'sha256').hexdigest() == row['sha256'], 'Signed asset checksum differs')


def validate_instance(root, target, compiled):
    pin = descriptor(target, compiled)
    path = root / 'consumer-bundle.json'
    require(path.stat().st_size <= 4*1024**2 and hashlib.sha256(path.read_bytes()).hexdigest() == pin['manifest_sha256'],
            'Signed Instance manifest differs from compiled descriptor')
    manifest, _ = module('package-observatory').instance_files(root, target)
    runtime = manifest['native_runtime']
    platform = 'macos-' + target.split('-')[0]
    require(manifest['schema'] == 1 and manifest['kind'] == 'instance-consumer-bundle'
            and manifest['source'] == pin['instance']['source'] and manifest['version'] == pin['instance']['version']
            and manifest['target'] == target and manifest['binary'] == 'agent-instance'
            and manifest.get('private_derivative') == {'schema':1, 'kind':'signed_macos_instance','cohort_id':pin['cohort_id']}
            and manifest['python_archive_sha256'] == pin['python']['archive_sha256'], 'Signed Instance identity differs')
    require(runtime == {'kind':'private_derivative','binary':'engine/native/agent-runtime',
             'version':pin['runtime']['version'],'binary_sha256':pin['runtime_binary_sha256'],'platform':platform,
             'public_origin':{'source':pin['runtime']['source'],'archive_sha256':pin['runtime']['archive_sha256'],
                              'binary_sha256':pin['runtime']['binary_sha256']}}, 'Signed Runtime identity differs')
    files = manifest['files']
    require(all(files.get(name) == digest for name, digest in {
        'agent-instance':pin['instance_binary_sha256'], 'engine/native/agent-runtime':pin['runtime_binary_sha256'],
        'setup/native-runtime-lock.json':pin['runtime']['lock_sha256'],
        'setup/private-python-lock.json':pin['python']['lock_sha256']}.items()), 'Signed Instance required member differs')
    actual = set()
    for path in root.rglob('*'):
        require(not path.is_symlink(), 'Signed Instance contains a link')
        if path.is_dir():
            require(any(path.iterdir()), 'Signed Instance contains an unlisted empty directory')
        else:
            require(path.is_file(), 'Signed Instance contains a special file')
            actual.add(path.relative_to(root).as_posix())
    require(actual == set(files) | {'consumer-bundle.json'}, 'Signed Instance has an unlisted member')


def validate_slack(path, target, slack_lock):
    release = module('fetch-slack-bundle').selected(target, slack_lock)['release']
    module('package-observatory').slack_files(path, target, slack_lock)
    with zipfile.ZipFile(path) as archive:
        total = 0
        seen = set()
        for member in archive.infolist():
            parts = member.filename.split('/')
            require(0 < len(member.filename) <= 1024 and len(parts) <= 32
                    and not any(part in ('', '.', '..') for part in parts)
                    and not any(c in member.filename for c in ('\\', ':', '\0'))
                    and member.filename.casefold() not in seen, 'Unsafe signed Slack member path')
            seen.add(member.filename.casefold())
            row = release['files'][member.filename]
            require(set(row) == {'size','sha256','mode'} and type(row['size']) is int
                    and 0 <= row['size'] <= MAX_BYTES and hex_value(row['sha256'])
                    and row['mode'] in (0o644, 0o755)
                    and stat.S_IFMT(member.external_attr >> 16) in (0, stat.S_IFREG)
                    and (member.external_attr >> 16) & 0o777 == row['mode']
                    and member.file_size == row['size'], 'Signed Slack member metadata differs')
            total += member.file_size; require(total <= MAX_BYTES, 'Excessive signed Slack inventory')
            require(hashlib.sha256(archive.read(member)).hexdigest() == row['sha256'], 'Signed Slack member differs')


def validate_helpers(path, helper):
    stage = module('stage-machine-helpers')
    manifest, files = stage.verified_files(path, helper['asset']['sha256'])
    require(hashlib.sha256(files['manifest.json']).hexdigest() == helper['manifest_sha256']
            and manifest.get('source') == helper['source']
            and {r['platform'] for r in manifest['artifacts']} == stage.PLATFORMS,
            'Retained helper closure identity differs')
    for row in manifest['artifacts']:
        require(row.get('source') == helper['source'] and row.get('version') == helper['version'],
                'Retained helpers do not share the reviewed source/version')
        packaging=module('package-machine-helpers')
        distribution=packaging.distribution({**row, 'signed_or_notarized':bool(row.get('signed_derivative'))}, row['platform'])
        if row['platform'].startswith('macos-'):
            require(row.get('bootstrap_delivery') == 'separate_file' and row.get('signed_derivative') is not None,
                    'Mac helper is missing signed separate-file provenance')
            with zipfile.ZipFile(io.BytesIO(files[row['filename']])) as nested:
                packaging.validate_signed_members(nested,row['platform'],distribution)
    return manifest


def fetch_helpers(output, *, lock_path=None, instance_lock=None, slack_lock=None, transport=None):
    cohort = selection(lock_path, instance_lock, slack_lock)
    require(cohort is not None, 'Signed build inputs are not armed')
    require(not output.exists() and not output.is_symlink(), 'Existing helper output was preserved')
    transport = transport or GitHubAssets(); verify_release(cohort, transport)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='signed-helper-', dir=output.parent) as temporary:
        path = Path(temporary) / 'helpers.zip'
        download(cohort['helpers']['asset'], path, transport); validate_helpers(path, cohort['helpers'])
        os.link(path, output)
    return {'source_kind':'retained_signed_owner_cohort','release_id':cohort['release_id'],
            'asset':cohort['helpers']['asset'], **{k:cohort['helpers'][k] for k in ('version','source','manifest_sha256')}}


def fetch_target(target, output, *, lock_path=None, instance_lock=None, slack_lock=None, transport=None):
    require(target in MAC, 'Signed owner inputs are Mac only')
    cohort = selection(lock_path, instance_lock, slack_lock)
    require(cohort is not None, 'Signed build inputs are not armed')
    require(not output.exists() and not output.is_symlink(), 'Existing signed target output was preserved')
    transport = transport or GitHubAssets(); verify_release(cohort, transport)
    output.parent.mkdir(parents=True, exist_ok=True)
    instance_lock = instance_lock or SCRIPTS / 'signed-instance-cohort-lock.json'
    slack_lock = slack_lock or SCRIPTS / 'slack-bundle-lock.json'
    with tempfile.TemporaryDirectory(prefix='signed-target-', dir=output.parent) as temporary:
        stage = Path(temporary)
        for component in ('instance', 'slack'):
            download(cohort[component][target], stage / (component + '.zip'), transport)
        module('fetch-instance-bundle').unpack(stage / 'instance.zip', stage / 'instance')
        validate_instance(stage / 'instance', target, load(instance_lock)['targets'][target])
        validate_slack(stage / 'slack.zip', target, slack_lock)
        output.mkdir() # Exclusive publication; no existing destination is repaired.
        shutil.copytree(stage / 'instance', output / 'instance')
        os.link(stage / 'slack.zip', output / 'slack.zip')
    return {'source_kind':'retained_signed_owner_cohort','target':target,'release_id':cohort['release_id'],
            'instance_asset':cohort['instance'][target],'slack_asset':cohort['slack'][target]}


def select_helpers(source_directory, output, source, helper_version, *, source_inputs_only=False,
                   lock_path=None, instance_lock=None, slack_lock=None, transport=None):
    require(hex_value(source, 40) and version(helper_version), 'Use the exact helper build source/version')
    require(not output.exists() and not output.is_symlink(), 'Existing helper closure was preserved')
    cohort = None if source_inputs_only else selection(lock_path, instance_lock, slack_lock)
    if cohort is None:
        module('prepare-helper-release').prepare(source_directory, output, helper_version, source)
        path = output / f'constellation-machine-helpers-{helper_version}.zip'
        with path.open('rb') as stream: digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        result = {'source_kind':'same_run_source_inputs','version':helper_version,'source':source,'sha256':digest}
    else:
        helper = cohort['helpers']
        path = output / f'constellation-machine-helpers-{helper["version"]}.zip'
        provenance = fetch_helpers(path, lock_path=lock_path, instance_lock=instance_lock,
                                   slack_lock=slack_lock, transport=transport)
        with path.with_suffix('.zip.sha256').open('x') as stream:
            stream.write(helper['asset']['sha256'] + '  ' + path.name + '\n')
        result = {'source_kind':provenance['source_kind'],'version':helper['version'],
                  'source':helper['source'],'sha256':helper['asset']['sha256'],'retained':provenance}
    with (output / 'signed-input-provenance.json').open('x') as stream:
        json.dump(result, stream, indent=2); stream.write('\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('inspect')
    select = sub.add_parser('select-helpers')
    select.add_argument('--source-directory', required=True, type=Path)
    select.add_argument('--output', required=True, type=Path)
    select.add_argument('--source', required=True)
    select.add_argument('--version', required=True)
    select.add_argument('--source-inputs-only', action='store_true')
    select.add_argument('--github-output', type=Path)
    helpers = sub.add_parser('helpers'); helpers.add_argument('--output', required=True, type=Path)
    target = sub.add_parser('target'); target.add_argument('--target', required=True, choices=sorted(MAC)); target.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    if args.command == 'inspect': result = {'armed':selection() is not None}
    elif args.command == 'select-helpers':
        result = select_helpers(args.source_directory, args.output, args.source, args.version,
                                source_inputs_only=args.source_inputs_only)
        if args.github_output is not None:
            with args.github_output.open('a') as stream:
                for key in ('version', 'source', 'sha256', 'source_kind'):
                    stream.write(key + '=' + result[key] + '\n')
    elif args.command == 'helpers': result = fetch_helpers(args.output)
    else: result = fetch_target(args.target, args.output)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
