#!/usr/bin/env python3
"""Validate supplied facts and emit a plan. No cluster or credential access."""
import argparse, hashlib, json, pathlib, re
from jsonschema import Draft202012Validator, FormatChecker
from common import pinned_schema

ROOT = pathlib.Path(__file__).resolve().parent
PIN = json.loads((ROOT / 'candidate.json').read_text())

def read(path):
    return json.loads(path.read_text())

def schema(name, value):
    Draft202012Validator(pinned_schema(name), format_checker=FormatChecker()).validate(value)

def require(condition, reason):
    if not condition:
        raise ValueError('STOP: ' + reason)

def render(command, binding, facts, receipt, expected, namespace):
    for field in ('parentApproved', 'admittedCommandExportVerified', 'environmentBindingExportVerified',
                  'isolatedDatabaseVerified', 'noOutboxDispatcherVerified', 'existingSecretVerified',
                  'existingServiceAccountVerified', 'existingNetworkEgressVerified', 'quotaHeadroomVerified',
                  'existingNamespaceVerified', 'existingPvcVerified'):
        require(facts.get(field) is True, field + ' is required')
    require(isinstance(namespace, str) and len(namespace) <= 63 and
            re.fullmatch(r'lw-bundle-[a-z0-9](?:[a-z0-9-]*[a-z0-9])?', namespace),
            'use the approved existing test namespace')
    require(facts.get('existingNamespace') == namespace, 'namespace differs from reviewed facts')
    require(isinstance(facts.get('databaseNameForReview'), str) and facts['databaseNameForReview'] not in
            ('', 'labweaver', 'postgres', 'template0', 'template1'), 'name the reviewed isolated test database')
    run = facts['runId']
    require(re.fullmatch(r'lw197-[a-z0-9][a-z0-9-]{0,23}', run), 'invalid test run ID')
    require(facts['phase'] in ('a', 'replay-a', 'b'), 'unknown phase')
    schema('environment-freeze-binding.schema.json', binding)
    request = command['request']
    require(set(command) == {'request', 'source'}, 'unknown worker command fields')
    require(set(request) == {'frozenSubmissionId', 'projectId', 'courseId', 'actorId', 'agentRunId',
                            'manifestRevision', 'manifest', 'environment', 'retention', 'idempotencyKey', 'traceId'},
            'request differs from the candidate FreezeRequest contract')
    manifest = request['manifest']
    schema('submission-manifest.schema.json', manifest)
    require(binding['source']['kind'] == 'container', 'VM/SFTP is not rendered by this PVC plan')
    require(request['environment'] == binding['environment'], 'environment identity differs from binding')
    require(request['agentRunId'] == binding['agentRunId'], 'AgentRun differs from binding')
    for field in ('projectId', 'actorId', 'frozenSubmissionId'):
        require(request[field] == facts[field], field + ' differs from reviewed admitted facts')
    require(not request['traceId'].startswith('LOCAL_PARSE_ONLY'), 'synthetic parse command cannot run')
    require(binding['environment']['runtimeKind'] == 'container', 'runtime kind is not container')
    require(binding['source']['namespace'] == namespace,
            'authoritative source binding is outside the approved test namespace; do not rewrite it')
    pvc_name = binding['source']['persistentVolumeClaim']
    require(isinstance(pvc_name, str) and len(pvc_name) <= 63 and
            re.fullmatch(r'[a-z0-9](?:[a-z0-9-]*[a-z0-9])?', pvc_name), 'invalid authoritative PVC name')
    source_hash = hashlib.sha256(json.dumps(binding['environment'], sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    require(command['source'] == {'kind': 'pvc', 'workspaceRoot': '/workspace', 'sourceIdentity': source_hash},
            'PVC source differs from the coordinator binding')
    paths = sorted(f['path'] for f in expected['a'])
    require(manifest['source'] == 'workspace' and manifest['followSymlinks'] is False, 'unsafe source selection')
    require(manifest['include'] == [{'kind': 'exactFile', 'path': path} for path in paths], 'selection is not the three synthetic files')
    require(manifest['exclude'] == [] and manifest['llmReadable'] == [], 'unexpected exclusion or LLM input')
    require(manifest['maxTotalBytes'] <= 67108864 and manifest['maxFiles'] <= 10000, 'service limits changed')
    require(request['retention']['class'] == 'student_submission', 'retention class differs from worker contract')
    require(receipt['candidateCommit'] == PIN['candidateCommit'] and receipt['candidateTree'] == PIN['candidateTree'], 'candidate differs')
    digest = receipt['ociManifestDigest']
    require(re.fullmatch(r'sha256:[0-9a-f]{64}', digest) and digest != 'sha256:' + '0' * 64, 'missing built OCI manifest digest')
    image = 'localhost/labweaver-197/evaluation-service@' + digest
    require(receipt['imageReference'] == image and receipt.get('nodeImportVerified') is True, 'node import is not verified')
    require(facts['nodeName'] in receipt.get('verifiedNodes', []), 'chosen node has no verified exact image')
    store = facts['objectStore']
    require(store['endpoint'].startswith('https://'), 'object store must use existing HTTPS')
    prefix = 'frozen-submissions/' + run
    require(store['objectPrefix'] == prefix and store['maxObjectBytes'] == 100663296, 'test object prefix or bound differs')
    require(store['caBundleFile'] == '/etc/labweaver/secrets/minio-ca.pem', 'CA file locator differs')
    name = run + '-freeze-' + facts['phase']
    config = {'databaseUrlFile': '/etc/labweaver/secrets/database-url', 'databaseMaxConnections': 2,
              'objectStore': store, 'objectStoreAccessKeyFile': '/etc/labweaver/secrets/minio-access-key',
              'objectStoreSecretKeyFile': '/etc/labweaver/secrets/minio-secret-key',
              'objectStoreSessionTokenFile': None, 'objectPrefix': prefix, 'workerId': name,
              'collectorLimits': {'maxSourceBytes': 67108864, 'maxArchiveBytes': 100663296, 'maxFiles': 10000}}
    labels = {'app.kubernetes.io/name': 'evaluation-freeze-worker', 'app.kubernetes.io/managed-by': 'labweaver-197-bundle',
              'labweaver.io/test-run': run, 'labweaver.io/test-scope': 'issue197-synthetic',
              'labweaver.io/frozen-submission-id': request['frozenSubmissionId']}
    metadata = {'name': name, 'namespace': namespace, 'labels': labels}
    mounts = [{'name': 'command', 'mountPath': '/etc/labweaver/worker', 'readOnly': True},
              {'name': 'secrets', 'mountPath': '/etc/labweaver/secrets', 'readOnly': True},
              {'name': 'workspace', 'mountPath': '/workspace', 'readOnly': True}]
    volumes = [{'name': 'command', 'configMap': {'name': name}},
               {'name': 'secrets', 'secret': {'secretName': facts['existingSecretName'], 'defaultMode': 288,
                'items': [{'key': facts['secretKeys'][key], 'path': path} for key, path in
                          [('databaseUrl', 'database-url'), ('accessKey', 'minio-access-key'),
                           ('secretKey', 'minio-secret-key'), ('ca', 'minio-ca.pem'),
                           ('postgresCa', 'postgres-ca.pem')]]}},
               {'name': 'workspace', 'persistentVolumeClaim': {'claimName': pvc_name, 'readOnly': True}}]
    cm = {'apiVersion': 'v1', 'kind': 'ConfigMap', 'metadata': metadata, 'immutable': True,
          'data': {'worker.json': json.dumps(config), 'command.json': json.dumps(command)}}
    job = {'apiVersion': 'batch/v1', 'kind': 'Job', 'metadata': metadata, 'spec': {'backoffLimit': 0, 'activeDeadlineSeconds': 900,
           'template': {'metadata': {'labels': labels}, 'spec': {'restartPolicy': 'Never',
           'serviceAccountName': facts['existingServiceAccount'], 'automountServiceAccountToken': False,
           'nodeSelector': {'kubernetes.io/hostname': facts['nodeName'], 'kubernetes.io/arch': 'amd64'},
           'securityContext': {'runAsNonRoot': True, 'runAsUser': 65532, 'runAsGroup': 65532, 'fsGroup': 65532,
                               'seccompProfile': {'type': 'RuntimeDefault'}},
           'containers': [{'name': 'freeze', 'image': image, 'imagePullPolicy': 'Never', 'args': ['--mode', 'freeze-worker'],
           'env': [{'name': 'LABWEAVER_EVALUATION_CONFIG_FILE', 'value': '/etc/labweaver/worker/worker.json'},
                   {'name': 'LABWEAVER_FREEZE_COMMAND_FILE', 'value': '/etc/labweaver/worker/command.json'},
                   {'name': 'SSL_CERT_FILE', 'value': '/etc/labweaver/secrets/minio-ca.pem'}],
           'terminationMessagePolicy': 'File', 'securityContext': {'allowPrivilegeEscalation': False,
           'readOnlyRootFilesystem': True, 'capabilities': {'drop': ['ALL']}},
           'resources': {'requests': {'cpu': '100m', 'memory': '128Mi'}, 'limits': {'cpu': '1', 'memory': '512Mi'}},
           'volumeMounts': mounts}], 'volumes': volumes}}}}
    return {'apiVersion': 'v1', 'kind': 'List', 'items': [cm, job]}

def main():
    p = argparse.ArgumentParser()
    for name in ('command', 'binding', 'facts', 'receipt', 'expected', 'output'):
        p.add_argument('--' + name, type=pathlib.Path, required=True)
    p.add_argument('--namespace', required=True, help='existing approved test namespace; never created by this script')
    a = p.parse_args()
    require(not a.output.exists(), 'output exists')
    plan = render(read(a.command), read(a.binding), read(a.facts), read(a.receipt), read(a.expected), a.namespace)
    a.output.write_text(json.dumps(plan, indent=2) + '\n')
    print('PLAN_WRITTEN_NOT_APPLIED ' + str(a.output))

if __name__ == '__main__':
    main()
