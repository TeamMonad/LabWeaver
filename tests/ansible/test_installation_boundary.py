"""Execute complete planning/ownership boundaries without a product-backend mock.

Kubernetes observations are the external transport fixture. Helm renders the real
application chart. SQL/NATS/identity binaries are fail-on-use tripwires: they never
pretend that a business write succeeded.
"""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import yaml

from test_deployment_install_modes import INSTALLATION, ROOT, named, probe_command, run_assertion, tasks
from test_deployment_ownership_execution import run_tasks
from test_infrastructure_cleanup_ownership import BOUNDARY as PATCH_BOUNDARY, localize_template_lookups, native_kubernetes_boundary

BOUNDARY = ROOT / 'deploy/ansible/roles/installation_boundary'
KUBE_READ = '''from ansible.module_utils.basic import AnsibleModule
import json, pathlib
module = AnsibleModule(argument_spec={
    'kubeconfig': {'type': 'str', 'required': True}, 'api_version': {'type': 'str', 'default': 'v1'},
    'kind': {'type': 'str', 'required': True}, 'name': {'type': 'str'}, 'namespace': {'type': 'str'},
    'label_selectors': {'type': 'list', 'elements': 'str'}, 'field_selectors': {'type': 'list', 'elements': 'str'},
}, supports_check_mode=True)
p = module.params
fixture = json.loads(pathlib.Path(p['kubeconfig'] + '.observations.json').read_text())
key = json.dumps([p['api_version'], p['kind'], p.get('namespace') or '', p.get('name') or ''])
with pathlib.Path(fixture['log']).open('a') as output:
    output.write(key + '\\n')
if key not in fixture['responses']:
    module.fail_json(msg='UNEXPECTED_KUBERNETES_READ ' + key)
response = fixture['responses'][key]
if 'error' in response:
    module.fail_json(msg=response['error'])
module.exit_json(changed=False, **response)
'''


def key(api, kind, namespace='', name=''):
    return json.dumps([api, kind, namespace or '', name or ''])


def observation(obj):
    return {'resources': [obj], 'api_found': True}


def owned(obj, uid):
    result = copy.deepcopy(obj)
    result.setdefault('metadata', {}).update({'uid': uid, 'resourceVersion': '17'})
    result['metadata'].setdefault('annotations', {})['labweaver.io/installation-id'] = INSTALLATION
    return result


def convert_kube_reads(value):
    if isinstance(value, list):
        return [convert_kube_reads(item) for item in value]
    if isinstance(value, dict):
        return {('boundary_k8s_info' if name == 'kubernetes.core.k8s_info' else name): convert_kube_reads(item)
                for name, item in value.items()}
    return value


def prepare_kube_transport(directory, responses):
    library = directory / 'library'
    library.mkdir()
    (library / 'boundary_k8s_info.py').write_text(KUBE_READ)
    kubeconfig = directory / 'kubeconfig'
    kubeconfig.write_text('apiVersion: v1\nkind: Config\nclusters: []\ncontexts: []\nusers: []\n')
    log = directory / 'reads.jsonl'
    Path(str(kubeconfig) + '.observations.json').write_text(json.dumps({'responses': responses, 'log': str(log)}))
    return kubeconfig, log, {'ANSIBLE_LIBRARY': str(library), 'LW_BUNDLE_INSTALLATION_ID': INSTALLATION}


def temporary_boundary_role(directory):
    role = directory / 'roles/installation_boundary'
    (role / 'tasks').mkdir(parents=True)
    (role / 'defaults').mkdir()
    (role / 'files').mkdir()
    for source in (BOUNDARY / 'tasks').glob('*.yml'):
        (role / 'tasks' / source.name).write_text(yaml.safe_dump(convert_kube_reads(yaml.safe_load(source.read_text()))))
    shutil.copyfile(BOUNDARY / 'defaults/main.yml', role / 'defaults/main.yml')
    shutil.copyfile(BOUNDARY / 'files/stamp-installation.py', role / 'files/stamp-installation.py')
    return role.parent


class InstallationBoundaryTests(unittest.TestCase):
    def test_actual_addon_namespace_producer_satisfies_all_installation_consumers(self):
        with tempfile.TemporaryDirectory() as temporary:
            capture = Path(temporary) / 'namespaces.json'
            template = ROOT / 'deploy/ansible/roles/cluster_addons/templates/gateway.yml.j2'
            producer = {'name': 'Render actual addon Namespace producers', 'ansible.builtin.copy': {
                'dest': str(capture), 'mode': '0600', 'content':
                "{{ lookup('template', '" + str(template) + "') | from_yaml_all | list | to_json }}"}}
            result = run_tasks([producer], {'labweaver_installation_id': INSTALLATION, 'gateway_vip': '10.0.0.10',
                'labweaver_namespaces': ['labweaver-system', 'labweaver-data', 'labweaver-build', 'labweaver-demo']})
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            namespaces = {obj['metadata']['name']: obj for obj in json.loads(capture.read_text()) if obj['kind'] == 'Namespace'}
            for role, name, guard_name in (
                ('platform_application', 'labweaver-system', 'Refuse to adopt an unowned application namespace'),
                ('resource_application', 'labweaver-system', 'Refuse to adopt an unowned Resource namespace'),
                ('platform_foundation', 'labweaver-data', 'Refuse to adopt an unowned foundation namespace'),
                ('platform_buildkit', 'labweaver-build', 'Refuse to adopt an unowned BuildKit namespace'),
            ):
                with self.subTest(consumer=role):
                    guard = named(tasks(f'deploy/ansible/roles/{role}/tasks/main.yml'), guard_name)
                    current = copy.deepcopy(namespaces[name])
                    variables = {'labweaver_installation_id': INSTALLATION, f'{role}_installation_id': INSTALLATION,
                                 f'{role}_namespace': name, f'{role}_kubeconfig': 'unused',
                                 f'{role}_existing_namespace': {'resources': [current]}}
                    accepted = run_assertion(guard, variables)
                    self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
                    current['metadata']['annotations']['labweaver.io/installation-id'] = 'foreign'
                    rejected = run_assertion(guard, variables)
                    self.assertNotEqual(rejected.returncode, 0, rejected.stdout + rejected.stderr)

    def test_network_crd_producer_is_explicit_and_covers_every_pinned_catalog_entry(self):
        source = tasks('deploy/ansible/roles/cluster_network/tasks/main.yml')
        bind = localize_template_lookups([named(source, 'Bind the exact Cilium values used by planning and reconciliation')], 'cluster_network')
        helper = tasks('deploy/ansible/roles/cluster_network/tasks/cilium-crds.yml')
        with tempfile.TemporaryDirectory() as temporary:
            capture = Path(temporary) / 'plan.json'
            record = {'name': 'Capture exact network producer plan', 'ansible.builtin.copy': {
                'dest': str(capture), 'mode': '0600', 'content':
                "{{ {'values': cluster_network_cilium_values, 'sources': cluster_network_cilium_crd_sources} | to_json }}"}}
            variables = {'labweaver_installation_id': INSTALLATION, 'kubernetes_api_advertise_address': '10.0.0.1',
                'metallb_pool': '10.0.0.10-10.0.0.20', 'cluster_network_host_firewall_cluster_cidr': '10.0.0.0/8',
                'cluster_network_cilium_lock': {'chart': '1.19.8', 'image': 'quay.io/cilium/cilium:v1.19.8@sha256:' + 'a' * 64},
                'cluster_network_cilium_crd_files': ['reviewed-v2.yaml', 'reviewed-v2alpha1.yaml']}
            result = run_tasks(bind + helper + [record], variables)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            plan = json.loads(capture.read_text())
            self.assertEqual(plan['sources'], variables['cluster_network_cilium_crd_files'])
            self.assertIs(plan['values']['operator']['skipCRDCreation'], True)
            self.assertIs(plan['values']['gatewayAPI']['gatewayClass']['create'], True)
            variables['cluster_network_cilium_crd_files'] = []
            variables['cluster_network_cilium_crd_catalogs'] = {'results': [
                {'item': 'v2', 'json': [{'type': 'file', 'name': 'ciliumnetworkpolicies.yaml'},
                                      {'type': 'file', 'name': 'ciliumnodes.yaml'}]},
                {'item': 'v2alpha1', 'json': [{'type': 'file', 'name': 'ciliumendpointslices.yaml'}]},
            ]}
            result = run_tasks(bind + helper[1:] + [record], variables)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            planned = json.loads(capture.read_text())['sources']
            self.assertEqual(len(planned), 3)
            for name in ('ciliumnetworkpolicies.yaml', 'ciliumnodes.yaml', 'ciliumendpointslices.yaml'):
                self.assertTrue(any(locator.endswith('/' + name) for locator in planned), planned)

    def test_mixed_release_reentry_verifies_each_sibling_and_accepts_both_uid_trees(self):
        entries = tasks('deploy/ansible/roles/installation_boundary/tasks/releases.yml')
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            fixture = directory / 'releases.json'
            helm = directory / 'helm'
            helm.write_text(f'#!{sys.executable}\nimport json, pathlib, sys\n'
                            f'data=json.loads(pathlib.Path({str(fixture)!r}).read_text())\n'
                            'if "list" in sys.argv: print(json.dumps([{"name": name} for name in data]))\n'
                            'else:\n'
                            ' name=sys.argv[sys.argv.index("metadata")+1]\n'
                            ' if isinstance(data[name], int): sys.exit(data[name])\n'
                            ' print(json.dumps(data[name]))\n')
            helm.chmod(0o755)
            release_names = ['labweaver', 'labweaver-resource']
            records = {name: {'name': name, 'namespace': 'labweaver-system',
                             'description': 'labweaver-installation:' + INSTALLATION} for name in release_names}
            variables = {'labweaver_installation_id': INSTALLATION, 'installation_boundary_helm': str(helm),
                         'installation_boundary_kubeconfig': 'unused', 'installation_boundary_release_namespace': 'labweaver-system'}
            capture = directory / 'verified.json'
            capture_task = {'name': 'Capture independently verified siblings', 'ansible.builtin.copy': {
                'dest': str(capture), 'mode': '0600', 'content': '{{ installation_boundary_verified_release_names | to_json }}'}}
            fixture.write_text(json.dumps(records))
            result = run_tasks(entries + [capture_task], variables, {'LW_BUNDLE_INSTALLATION_ID': INSTALLATION})
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            verified = json.loads(capture.read_text())
            self.assertEqual(verified, release_names)
            objects = []
            for index, release in enumerate(release_names):
                deployment = {'apiVersion': 'apps/v1', 'kind': 'Deployment', 'metadata': {
                    'name': release, 'namespace': 'labweaver-system', 'uid': f'deployment-{index}',
                    'labels': {'app.kubernetes.io/managed-by': 'Helm'},
                    'annotations': {'meta.helm.sh/release-name': release, 'meta.helm.sh/release-namespace': 'labweaver-system'}}}
                replica = {'apiVersion': 'apps/v1', 'kind': 'ReplicaSet', 'metadata': {
                    'name': release + '-replica', 'uid': f'replica-{index}', 'ownerReferences': [{'uid': f'deployment-{index}'}]}}
                pod = {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': release + '-pod',
                       'uid': f'pod-{index}', 'ownerReferences': [{'uid': f'replica-{index}'}]}}
                storage = {'apiVersion': 'v1', 'kind': 'Secret', 'type': 'helm.sh/release.v1',
                           'metadata': {'name': f'sh.helm.release.v1.{release}.v1', 'labels': {'owner': 'helm', 'name': release}}}
                objects.extend([deployment, replica, pod, storage])
            for prefix in ('platform_application', 'resource_application'):
                with self.subTest(role=prefix):
                    probe = next(task for task in tasks(f'deploy/ansible/roles/{prefix}/tasks/main.yml')
                                 if task.get('name', '').startswith('Refuse to adopt unowned') and 'objects' in task['name'].lower())
                    rendered = probe_command(probe, {f'{prefix}_kubectl': 'kubectl', f'{prefix}_kubeconfig': 'config',
                        f'{prefix}_namespace': 'labweaver-system', f'{prefix}_installation_id': INSTALLATION,
                        f'{prefix}_release_name': release_names[0 if prefix == 'platform_application' else 1],
                        'installation_boundary_verified_release_names': verified})
                    program = rendered.split('jq -e ', 1)[1].strip().strip("'")
                    accepted = subprocess.run(['jq', '-e', program], input=json.dumps({'items': objects}), text=True,
                                              capture_output=True, check=False)
                    self.assertEqual(accepted.returncode, 0, accepted.stderr)
            for mutation in ('foreign-installation', 'wrong-namespace', 'read-error'):
                with self.subTest(sibling=mutation):
                    foreign = copy.deepcopy(records)
                    if mutation == 'foreign-installation':
                        foreign['labweaver-resource']['description'] = 'labweaver-installation:foreign'
                    elif mutation == 'wrong-namespace':
                        foreign['labweaver-resource']['namespace'] = 'foreign'
                    else:
                        foreign['labweaver-resource'] = 2
                    fixture.write_text(json.dumps(foreign))
                    result = run_tasks(entries, variables, {'LW_BUNDLE_INSTALLATION_ID': INSTALLATION})
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_final_real_chart_conflict_leads_to_zero_sql_nats_minio_or_identity_writes(self):
        main = tasks('deploy/ansible/roles/platform_application/tasks/main.yml')
        chart_guard = tasks('deploy/ansible/roles/platform_application/tasks/chart-ownership.yml')
        guard_name = 'Verify every final Helm mutation target before Kubernetes reconciliation'
        writer_names = {'Apply or verify exact six-domain baseline', 'Apply or verify Access service identities and memberships',
                        'Create only missing configured streams', 'Enable versioning without removing existing objects',
                        'Import reviewed realm only when absent'}
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            chart = directory / 'run/chart'
            shutil.copytree(ROOT / 'deploy/helm/labweaver', chart)
            values = yaml.safe_load((chart / 'values.yaml').read_text())
            for configuration in values['workloads'].values():
                configuration['enabled'] = False
            values['workloads']['resource-service']['enabled'] = True
            values['images']['resource_service'] = 'example.invalid/resource@sha256:' + 'a' * 64
            values['deploymentIdentity']['configurationBundleSha256'] = 'sha256:' + 'b' * 64
            values['internalIdentityProxy']['enabled'] = False
            for option in ('defaultDenyEnabled', 'cdiCloneSourcePolicyEnabled', 'ciliumIngressPolicyEnabled',
                           'resourceKubernetesApiCiliumPolicyEnabled'):
                values['network'][option] = False
            values['network']['kubernetesApiCidrs'] = ['10.0.0.1/32']
            values['network']['kubernetesApiPorts'] = [6443]
            values_path = directory / 'values.yaml'
            values_path.write_text(yaml.safe_dump(values))
            helm = shutil.which('helm')
            self.assertIsNotNone(helm)
            rendered = subprocess.run([helm, 'template', 'labweaver', str(chart), '--namespace', 'labweaver-system',
                                       '--values', str(values_path)], text=True, capture_output=True, check=False)
            self.assertEqual(rendered.returncode, 0, rendered.stderr)
            objects = [obj for obj in yaml.safe_load_all(rendered.stdout) if obj]
            self.assertTrue(any(obj['kind'] == 'ClusterRole' for obj in objects))
            responses = {}
            namespaces = {obj['metadata']['namespace'] for obj in objects if 'namespace' in obj['metadata']}
            for namespace in namespaces:
                responses[key('v1', 'Namespace', name=namespace)] = observation(owned({
                    'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': namespace}}, 'namespace-' + namespace))
            foreign_name = None
            for index, obj in enumerate(objects):
                current = owned(obj, 'root-' + str(index))
                if obj['kind'] == 'ClusterRole' and foreign_name is None:
                    foreign_name = obj['metadata']['name']
                    current['metadata']['annotations']['labweaver.io/installation-id'] = 'foreign'
                responses[key(obj['apiVersion'], obj['kind'], obj['metadata'].get('namespace'), obj['metadata']['name'])] = observation(current)
            kubeconfig, reads, environment = prepare_kube_transport(directory, responses)
            writes = directory / 'business-writes.jsonl'
            tripwire = directory / 'external-write'
            tripwire.write_text(f'#!{sys.executable}\nimport json, pathlib, sys\n'
                                f'with pathlib.Path({str(writes)!r}).open("a") as output: output.write(json.dumps(sys.argv) + "\\n")\n'
                                'raise SystemExit(97)\n')
            tripwire.chmod(0o755)
            stream = []
            for task in main:
                if task.get('name') == guard_name:
                    stream.append({'name': guard_name, 'block': convert_kube_reads(chart_guard)})
                elif task.get('name') in writer_names:
                    stream.append(copy.deepcopy(task))
            self.assertEqual(len(stream), len(writer_names) + 1)
            variables = {
                'platform_application_helm': helm, 'platform_application_kubeconfig': str(kubeconfig),
                'platform_application_release_name': 'labweaver', 'platform_application_namespace': 'labweaver-system',
                'platform_application_installation_id': INSTALLATION, 'platform_application_work_root': str(directory),
                'platform_application_run_id': 'run', 'platform_application_values_argv': ['--values', str(values_path)],
                'platform_application_image_argv': [], 'platform_application_route_argv': [],
                'platform_application_psql': str(tripwire), 'platform_application_nats': str(tripwire),
                'platform_application_mc': str(tripwire), 'platform_application_kcadm': str(tripwire),
                'platform_application_postgres_service': 'reviewed', 'platform_application_postgres_effective_service_file': 'unused',
                'platform_application_postgres_client_certificate_file': 'unused', 'platform_application_postgres_client_private_key_file': 'unused',
                'platform_application_runtime_passwords': {domain: 'unreachable' for domain in ('control', 'access', 'environment', 'agent', 'evaluation', 'resource')},
                'platform_application_nats_probe_server': 'tls://unused', 'platform_application_nats_credentials_file': 'unused',
                'platform_application_nats_ca_file': 'unused', 'platform_application_nats_client_certificate_file': 'unused',
                'platform_application_nats_client_private_key_file': 'unused', 'platform_application_stream_info_before': {'results': [
                    {'rc': 1, 'item': {'name': 'reviewed', 'subjects': ['reviewed.>'], 'max_age': '24h'}}]},
                'platform_application_minio_alias': 'reviewed', 'platform_application_minio_bucket': 'reviewed',
                'platform_application_minio_ca_file': 'unused', 'platform_application_keycloak_admin_token': 'unreachable',
                'platform_application_keycloak_config': 'unused', 'platform_application_keycloak_realm_before': {'rc': 1},
            }
            result = run_tasks(stream, variables, environment)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('Reject foreign final chart mutation targets', result.stdout)
            self.assertTrue(reads.exists(), result.stdout + result.stderr)
            self.assertIn(foreign_name, reads.read_text())
            self.assertFalse(writes.exists(), 'a final-chart conflict must prevent every business write')

    def test_same_name_infrastructure_release_checks_identity_before_any_producer(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            chart = directory / 'chart'
            (chart / 'templates').mkdir(parents=True)
            (chart / 'Chart.yaml').write_text('apiVersion: v2\nname: component\nversion: 1.0.0\n')
            target = {'apiVersion': 'apps/v1', 'kind': 'Deployment', 'metadata': {'name': 'operator', 'namespace': 'component'}}
            (chart / 'templates/root.yaml').write_text(yaml.safe_dump(target))
            namespace = {'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': 'component'}}
            chart_namespace = {'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': 'component-secrets',
                               'labels': {'app.kubernetes.io/part-of': 'component'}}}
            (chart / 'templates/namespace.yaml').write_text(yaml.safe_dump(chart_namespace))
            responses = {key('v1', 'Namespace', name='component'): observation(owned(namespace, 'namespace-uid')),
                         key('apps/v1', 'Deployment', 'component', 'operator'): observation(owned(target, 'operator-uid'))}
            responses[key('v1', 'Namespace', name='component-secrets')] = {'resources': [], 'api_found': True}
            kubeconfig, reads, environment = prepare_kube_transport(directory, responses)
            roles = temporary_boundary_role(directory)
            environment['ANSIBLE_ROLES_PATH'] = str(roles) + os.pathsep + str(ROOT / 'deploy/ansible/roles')
            namespace_capture, kubectl = directory / 'created-namespace.json', directory / 'kubectl'
            kubectl.write_text(f'#!{sys.executable}\nimport pathlib, sys\n'
                               'assert sys.argv[-3:] == ["create", "-f", "-"]\n'
                               f'pathlib.Path({str(namespace_capture)!r}).write_text(sys.stdin.read())\n')
            kubectl.chmod(0o755)
            environment['PATH'] = str(directory) + os.pathsep + os.environ['PATH']
            fixture = directory / 'release.json'
            helm = directory / 'helm'
            real_helm = shutil.which('helm')
            self.assertIsNotNone(real_helm)
            helm.write_text(f'#!{sys.executable}\nimport json, os, pathlib, sys\n'
                            f'data=json.loads(pathlib.Path({str(fixture)!r}).read_text())\n'
                            'if "list" in sys.argv:\n'
                            ' if data.get("list_error"): sys.exit(2)\n'
                            ' print(json.dumps([{"name": "component"}]))\n'
                            'elif "metadata" in sys.argv: print(json.dumps(data["metadata"]))\n'
                            'elif "values" in sys.argv: print(json.dumps(data["values"]))\n'
                            f'else: os.execv({real_helm!r}, [{real_helm!r}] + sys.argv[1:])\n')
            helm.chmod(0o755)
            producer = directory / 'post-renderer.py'
            variables = {'labweaver_installation_id': INSTALLATION, 'installation_boundary_kubeconfig': str(kubeconfig),
                         'installation_boundary_helm': str(helm), 'installation_boundary_post_renderer_path': str(producer),
                         'installation_boundary_helm_releases': [{'name': 'component', 'namespace': 'component',
                            'chart_ref': str(chart), 'chart_version': '1.0.0', 'values': {}}]}
            entry = {'name': 'Execute complete infrastructure Helm boundary', 'ansible.builtin.include_role': {'name': 'installation_boundary'}}
            valid = {'metadata': {'name': 'component', 'namespace': 'component'}, 'values': {'labweaverInstallationId': INSTALLATION}}
            fixture.write_text(json.dumps(valid))
            result = run_tasks([entry], variables, environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(producer.exists())
            namespace_metadata = json.loads(namespace_capture.read_text())['metadata']
            self.assertEqual(namespace_metadata['annotations']['labweaver.io/installation-id'], INSTALLATION)
            self.assertEqual(namespace_metadata['annotations']['meta.helm.sh/release-name'], 'component')
            self.assertEqual(namespace_metadata['annotations']['meta.helm.sh/release-namespace'], 'component')
            self.assertEqual(namespace_metadata['labels']['app.kubernetes.io/managed-by'], 'Helm')
            namespace_capture.unlink()
            producer.unlink()
            for variant in ('foreign-installation', 'wrong-namespace', 'read-error'):
                with self.subTest(release=variant):
                    invalid = copy.deepcopy(valid)
                    if variant == 'foreign-installation':
                        invalid['values']['labweaverInstallationId'] = 'foreign'
                    elif variant == 'wrong-namespace':
                        invalid['metadata']['namespace'] = 'foreign'
                    else:
                        invalid['list_error'] = True
                    fixture.write_text(json.dumps(invalid))
                    result = run_tasks([entry], variables, environment)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertFalse(producer.exists(), 'foreign or unreadable release must stop before producer staging')
                    self.assertFalse(namespace_capture.exists(), 'foreign or unreadable release must prevent every namespace creation')

    def test_chart_namespace_creation_has_exact_helm_identity_and_never_stamps_existing_namespaces(self):
        source = tasks('deploy/ansible/roles/installation_boundary/tasks/main.yml')
        producer = named(source, 'Create only verified-absent infrastructure namespaces after complete validation')
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            command, capture = directory / 'kubectl', directory / 'namespace.json'
            command.write_text(f'#!{sys.executable}\nimport pathlib, sys\n'
                               'assert sys.argv[-3:] == ["create", "-f", "-"]\n'
                               f'pathlib.Path({str(capture)!r}).write_text(sys.stdin.read())\n')
            command.chmod(0o755)
            variables = {'labweaver_installation_id': INSTALLATION, 'installation_boundary_kubeconfig': 'unused',
                'installation_boundary_manifest_objects': [], 'installation_boundary_chart_objects': [
                    {'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': 'cilium-secrets',
                     'labels': {'app.kubernetes.io/part-of': 'cilium'}}}],
                'installation_boundary_chart_namespace_metadata': {'cilium-secrets': {
                    'annotations': {'meta.helm.sh/release-name': 'cilium', 'meta.helm.sh/release-namespace': 'kube-system'},
                    'labels': {'app.kubernetes.io/managed-by': 'Helm'}}},
                'installation_boundary_namespace_observations': {'results': [{'item': 'cilium-secrets', 'resources': []}]}}
            environment = {'PATH': str(directory) + os.pathsep + os.environ['PATH']}
            result = run_tasks([producer], variables, environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            metadata = json.loads(capture.read_text())['metadata']
            self.assertEqual(metadata['annotations']['labweaver.io/installation-id'], INSTALLATION)
            self.assertEqual(metadata['annotations']['meta.helm.sh/release-name'], 'cilium')
            self.assertEqual(metadata['annotations']['meta.helm.sh/release-namespace'], 'kube-system')
            self.assertEqual(metadata['labels']['app.kubernetes.io/managed-by'], 'Helm')
            self.assertEqual(metadata['labels']['app.kubernetes.io/part-of'], 'cilium')
            capture.unlink()
            variables['installation_boundary_namespace_observations']['results'][0]['resources'] = [{'metadata': {'name': 'cilium-secrets'}}]
            result = run_tasks([producer], variables, environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(capture.exists())

    def test_infrastructure_full_inventory_and_post_renderer_produce_verified_roots(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            target = {'apiVersion': 'apps/v1', 'kind': 'Deployment', 'metadata': {'name': 'operator', 'namespace': 'component'}}
            namespace = {'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': 'component'}}
            responses = {key('v1', 'Namespace', name='component'): observation(owned(namespace, 'namespace-uid')),
                         key('apps/v1', 'Deployment', 'component', 'operator'): observation(owned(target, 'operator-uid'))}
            kubeconfig, reads, environment = prepare_kube_transport(directory, responses)
            roles = temporary_boundary_role(directory)
            environment['ANSIBLE_ROLES_PATH'] = str(roles) + os.pathsep + str(ROOT / 'deploy/ansible/roles')
            capture = directory / 'owned.json'
            include = {'name': 'Execute complete infrastructure guard', 'ansible.builtin.include_role': {'name': 'installation_boundary'}}
            record = {'name': 'Capture verified producer output', 'ansible.builtin.copy': {
                'dest': str(capture), 'mode': '0600', 'content': '{{ installation_boundary_owned_objects | to_json }}'}}
            variables = {'labweaver_installation_id': INSTALLATION, 'installation_boundary_kubeconfig': str(kubeconfig),
                         'installation_boundary_objects': [target]}
            result = run_tasks([include, record], variables, environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(capture.read_text())[0]['metadata']['annotations']['labweaver.io/installation-id'], INSTALLATION)
            responses[key('apps/v1', 'Deployment', 'component', 'operator')]['resources'][0]['metadata']['annotations']['labweaver.io/installation-id'] = 'foreign'
            Path(str(kubeconfig) + '.observations.json').write_text(json.dumps({'responses': responses, 'log': str(reads)}))
            capture.unlink()
            result = run_tasks([include, record], variables, environment)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(capture.exists())
        script = BOUNDARY / 'files/stamp-installation.py'
        for installation, accepted in ((INSTALLATION, True), ('', False)):
            result = subprocess.run([sys.executable, str(script)], input=yaml.safe_dump_all([namespace, target]),
                env=dict(os.environ, LW_BUNDLE_INSTALLATION_ID=installation), text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode == 0, accepted, result.stderr)
            if accepted:
                for obj in yaml.safe_load_all(result.stdout):
                    self.assertEqual(obj['metadata']['annotations']['labweaver.io/installation-id'], INSTALLATION)
        target['metadata']['annotations'] = {'labweaver.io/installation-id': 'foreign'}
        result = subprocess.run([sys.executable, str(script)], input=yaml.safe_dump(target),
            env=dict(os.environ, LW_BUNDLE_INSTALLATION_ID=INSTALLATION), text=True, capture_output=True, check=False)
        self.assertNotEqual(result.returncode, 0)

    def test_controller_generated_infrastructure_credentials_resolve_by_uid_not_name(self):
        guard = named(tasks('deploy/ansible/roles/installation_boundary/tasks/main.yml'),
                      'Refuse to overwrite unowned infrastructure mutation targets')
        root = owned({'apiVersion': 'apps/v1', 'kind': 'Deployment', 'metadata': {'name': 'metallb-controller'}}, 'deployment-uid')
        secret = {'apiVersion': 'v1', 'kind': 'Secret', 'metadata': {'name': 'metallb-memberlist', 'uid': 'secret-uid',
                  'ownerReferences': [{'uid': 'deployment-uid'}]}}
        for parent_uid, accepted in (('deployment-uid', True), ('replaced-deployment', False), ('secret-uid', False)):
            with self.subTest(owner=parent_uid):
                secret['metadata']['ownerReferences'][0]['uid'] = parent_uid
                result = run_tasks([guard], {'labweaver_installation_id': INSTALLATION,
                    'installation_boundary_object_observations': {'results': [{'resources': [root]}, {'resources': [secret]}]}})
                self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)

    def test_dls_execution_requires_the_exact_owned_deployment_replica_pod_uid_chain(self):
        guard = named(tasks('deploy/ansible/roles/fastapi_dls/tasks/main.yml'),
                      'Refuse DLS execution Pods outside the verified deployment UID chain')
        deployment = owned({'apiVersion': 'apps/v1', 'kind': 'Deployment', 'metadata': {'name': 'fastapi-dls'}}, 'deployment-uid')
        replica = {'metadata': {'uid': 'replica-uid', 'ownerReferences': [{'uid': 'deployment-uid'}]}}
        pod = {'metadata': {'uid': 'pod-uid', 'ownerReferences': [{'uid': 'replica-uid'}]}}
        variables = {'labweaver_installation_id': INSTALLATION, 'fastapi_dls_enabled': True,
                     'fastapi_dls_deployment_status': {'resources': [deployment]}, 'fastapi_dls_backend_pods': {'resources': [pod]},
                     'fastapi_dls_backend_replicasets': {'resources': [replica]}}
        for parent_uid, accepted in (('replica-uid', True), ('replaced-replica-uid', False)):
            pod['metadata']['ownerReferences'][0]['uid'] = parent_uid
            result = run_tasks([guard], variables)
            self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
        pod['metadata']['ownerReferences'][0]['uid'] = 'replica-uid'
        deployment['metadata']['annotations']['labweaver.io/installation-id'] = 'foreign'
        result = run_tasks([guard], variables)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_kubevirt_labels_use_bounded_installation_identity_and_atomic_uid_revision_tests(self):
        source = tasks('deploy/ansible/roles/kubevirt/tasks/main.yml')
        guard = named(source, 'Refuse to claim a KubeVirt node label owned by another installation')
        patch = named(source, 'Label selected KVM-capable workers for KubeVirt scheduling')
        label = 'labweaver.io/kubevirt-worker'
        node = {'apiVersion': 'v1', 'kind': 'Node', 'metadata': {'name': 'worker-1', 'uid': 'node-uid', 'resourceVersion': '17',
                'labels': {'unrelated': 'preserve'}, 'annotations': {}}}
        variables = {'labweaver_installation_id': INSTALLATION, 'kubevirt_node_label': label,
                     'kubevirt_node_ownership': {'results': [{'resources': [node]}]}}
        self.assertEqual(run_assertion(guard, variables).returncode, 0)
        node['metadata']['labels'][label] = 'true'
        for owner, accepted in (('', False), ('foreign', False), (INSTALLATION, True)):
            node['metadata']['annotations']['labweaver.io/kubevirt-installation-id'] = owner
            result = run_assertion(guard, variables)
            self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            command, capture = directory / 'patch.py', directory / 'mutation.json'
            command.write_text(PATCH_BOUNDARY)
            native = native_kubernetes_boundary(patch, command)
            for field, accepted in (('unchanged', True), ('uid', False), ('resourceVersion', False)):
                current = copy.deepcopy(node)
                if field != 'unchanged':
                    current['metadata'][field] = 'replaced'
                result = run_tasks([native], variables, {'CURRENT_OBJECT': json.dumps(current), 'CAPTURE': str(capture)})
                self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
                self.assertEqual(capture.exists(), accepted)
                if accepted:
                    updated = json.loads(capture.read_text())['object']
                    self.assertEqual(updated['metadata']['labels']['unrelated'], 'preserve')
                    self.assertEqual(updated['metadata']['annotations']['labweaver.io/kubevirt-installation-id'], INSTALLATION)
                    capture.unlink()


if __name__ == '__main__':
    unittest.main()
