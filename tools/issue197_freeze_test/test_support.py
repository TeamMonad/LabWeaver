"""Synthetic checks. No cluster, credentials, image build or database access."""
import base64
import copy
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import render_worker
from common import SUPPORT_ROOT, pinned_schema


class FreezeTestSupport(unittest.TestCase):
    def setUp(self):
        self.uid = lambda i: f'01970000-0000-7000-8000-{i:012d}'
        self.manifest = json.loads((SUPPORT_ROOT / 'fixtures/manifest.json').read_text())
        self.expected = json.loads((SUPPORT_ROOT / 'fixtures/expected-files.json').read_text())
        env = {'environmentId': self.uid(1), 'environmentRevision': 1, 'releaseId': self.uid(2),
               'releaseVersion': 1, 'runtimeKind': 'container', 'buildRequestId': self.uid(3)}
        digest = hashlib.sha256(json.dumps(env, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        self.request = {'frozenSubmissionId': self.uid(5), 'projectId': self.uid(6), 'courseId': None,
                        'actorId': self.uid(7), 'agentRunId': self.uid(8), 'manifestRevision': 1,
                        'manifest': self.manifest, 'environment': env,
                        'retention': {'policyId': self.uid(4), 'policyRevision': 1, 'class': 'student_submission',
                                      'retainUntil': '2027-01-12T00:00:00.000Z', 'disposition': 'delete'},
                        'idempotencyKey': 'SYNTHETIC_SELF_TEST_ONLY', 'traceId': 'SYNTHETIC_RENDER_SELF_TEST_ONLY'}
        self.command = {'request': self.request, 'source': {'kind': 'pvc', 'workspaceRoot': '/workspace', 'sourceIdentity': digest}}
        self.binding = {'environment': env, 'agentRunId': self.request['agentRunId'], 'source': {'kind': 'container',
                        'namespace': 'lw-env-' + self.uid(1), 'persistentVolumeClaim': 'workspace',
                        'storageClassName': 'SYNTHETIC_SELF_TEST_ONLY'}}
        self.facts = json.loads((SUPPORT_ROOT / 'facts.example.json').read_text())
        self.receipt = json.loads((SUPPORT_ROOT / 'image-receipt.example.json').read_text())

    def approve_synthetic_shape(self):
        for key in self.facts:
            if isinstance(self.facts[key], bool):
                self.facts[key] = True
        self.facts.update(projectId=self.request['projectId'], actorId=self.request['actorId'],
                          frozenSubmissionId=self.request['frozenSubmissionId'], nodeName='synthetic-node',
                          existingServiceAccount='synthetic-account', existingSecretName='synthetic-secret',
                          databaseNameForReview='lw197_synthetic')
        self.receipt.update(ociManifestDigest='sha256:' + '1' * 64,
                            imageReference='localhost/labweaver-197/evaluation-service@sha256:' + '1' * 64,
                            nodeImportVerified=True, verifiedNodes=['synthetic-node'])

    def render(self):
        return render_worker.render(self.command, self.binding, self.facts, self.receipt, self.expected)

    def test_existing_schemas_are_pinned_and_fixture_hashes_match(self):
        for name in ['submission-manifest.schema.json', 'environment-freeze-binding.schema.json', 'frozen-submission.schema.json']:
            self.assertIsInstance(pinned_schema(name), dict)
        for phase in ['a', 'b']:
            for file in self.expected[phase]:
                data = (SUPPORT_ROOT / ('fixtures/workspace-' + phase) / file['path']).read_bytes()
                self.assertEqual(len(data), file['sizeBytes'])
                self.assertEqual(hashlib.sha256(data).hexdigest(), file['sha256'])
        self.assertEqual([f['sizeBytes'] for f in self.expected['a']], [f['sizeBytes'] for f in self.expected['b']])
        self.assertNotEqual(self.expected['a'], self.expected['b'])

    def test_default_missing_approval_and_parse_identity_are_refused(self):
        with self.assertRaisesRegex(ValueError, 'parentApproved'):
            self.render()
        self.approve_synthetic_shape()
        self.request['traceId'] = 'LOCAL_PARSE_ONLY_NOT_AN_ADMITTED_COMMAND'
        with self.assertRaisesRegex(ValueError, 'synthetic parse'):
            self.render()

    def test_wrong_source_identity_image_or_existing_egress_is_refused(self):
        self.approve_synthetic_shape()
        self.facts['existingNetworkEgressVerified'] = False
        with self.assertRaisesRegex(ValueError, 'existingNetworkEgressVerified'):
            self.render()
        self.facts['existingNetworkEgressVerified'] = True
        self.command['source']['sourceIdentity'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'source differs'):
            self.render()
        self.command['source']['sourceIdentity'] = hashlib.sha256(json.dumps(self.binding['environment'], sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        self.receipt['nodeImportVerified'] = False
        with self.assertRaisesRegex(ValueError, 'node import'):
            self.render()

    def test_plan_has_only_cm_job_existing_refs_and_readonly_no_gpu_worker(self):
        self.approve_synthetic_shape()
        plan = self.render()
        self.assertEqual([x['kind'] for x in plan['items']], ['ConfigMap', 'Job'])
        pod = plan['items'][1]['spec']['template']['spec']
        self.assertFalse(pod['automountServiceAccountToken'])
        self.assertEqual(pod['containers'][0]['args'], ['--mode', 'freeze-worker'])
        self.assertEqual(pod['containers'][0]['imagePullPolicy'], 'Never')
        self.assertTrue(all(m['readOnly'] for m in pod['containers'][0]['volumeMounts']))
        self.assertFalse(any('gpu' in key.lower() for values in pod['containers'][0]['resources'].values() for key in values))
        self.assertEqual(len(pod['volumes'][1]['secret']['items']), 5)

    def test_fixture_generation_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'new'
            args = [sys.executable, str(SUPPORT_ROOT / 'fixtures.py'), '--run-id', 'lw197-local', '--output', str(path)]
            good = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(good.returncode, 0, good.stderr)
            refused = subprocess.run(args, capture_output=True, text=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn('already exists', refused.stderr)

    def test_export_verifier_checks_bytes_and_refuses_optimized_python(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            raw = json.dumps({'apiVersion': 'evaluation.labweaver.io/frozen-submission-archive/v1', 'files':
                             [{'path': f['path'], 'contentBase64': base64.b64encode((SUPPORT_ROOT / 'fixtures/workspace-a' / f['path']).read_bytes()).decode()}
                              for f in self.expected['a']]}, sort_keys=True, separators=(',', ':')).encode()
            sub = {**{k: self.request[k] for k in ['projectId', 'courseId', 'actorId', 'agentRunId', 'manifestRevision', 'environment', 'retention']},
                   'id': self.uid(5), 'attempt': 1, 'files': [dict(f, mediaType='text/plain') for f in self.expected['a']],
                   'object': {'artifactId': self.uid(9), 'storeBinding': 'SYNTHETIC_ONLY', 'objectVersion': 'SYNTHETIC_ONLY',
                              'sizeBytes': len(raw), 'mediaType': 'application/vnd.labweaver.frozen-submission.v1+json'},
                   'contentSha256': hashlib.sha256(raw).hexdigest(), 'systemFacts': {},
                   'frozenAt': '2026-10-12T00:00:00.000Z', 'derivedArchive': None}
            (path / 'sub.json').write_text(json.dumps(sub)); (path / 'archive.json').write_bytes(raw)
            args = [str(SUPPORT_ROOT / 'verify_frozen.py'), '--submission', str(path / 'sub.json'),
                    '--archive', str(path / 'archive.json'), '--expected', str(SUPPORT_ROOT / 'fixtures/expected-files.json'), '--phase', 'a']
            good = subprocess.run([sys.executable, *args], capture_output=True, text=True)
            self.assertEqual(good.returncode, 0, good.stderr)
            (path / 'archive.json').write_bytes(raw + b' ')
            corrupt = subprocess.run([sys.executable, *args], capture_output=True, text=True)
            self.assertNotEqual(corrupt.returncode, 0)
            optimized = subprocess.run([sys.executable, '-O', *args], capture_output=True, text=True)
            self.assertNotEqual(optimized.returncode, 0)
            self.assertIn('disable verification', optimized.stderr)


if __name__ == '__main__':
    unittest.main()
